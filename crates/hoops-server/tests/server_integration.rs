use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use futures_util::{SinkExt, StreamExt};
use hoops_config::ConfigBundle;
use hoops_core::{CanonicalLeBytes, MatchInitSpecV1, PlayerCoachCommand, StateHashMode};
use hoops_output::{decode_soa_zstd_archive, HReplayContainer};
use http_body_util::BodyExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message as WsMsg;
use tower::ServiceExt;

use hoops_server::archive_store::ArchiveStore;
use hoops_server::quota::ByteQuotaPool;
use hoops_server::{
    build_router, build_state, ArchiveStatus, CasOutcome, ServerConfig, SharedDurableWalStore,
};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

struct TempTestDir(PathBuf);

impl TempTestDir {
    fn new(label: &str) -> Self {
        let p = repo_root()
            .join("target/server-tests")
            .join(format!("{label}_{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempTestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn test_config(name: &str) -> (ServerConfig, TempTestDir) {
    let tmp = TempTestDir::new(name);
    let root = repo_root();
    let cfg = ServerConfig {
        bind: "127.0.0.1:0".to_owned(),
        configs_dir: root.join("configs"),
        viewer_dir: root.join("viewer"),
        data_dir: tmp.path().to_path_buf(),
        jwt_secret: "test-secret-key".to_owned(),
    };
    (cfg, tmp)
}

async fn http_req(
    addr: SocketAddr,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    json_body: Option<&str>,
) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let body = json_body.unwrap_or("");
    let auth_hdr = bearer
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let ct_hdr = if json_body.is_some() {
        "Content-Type: application/json\r\n"
    } else {
        ""
    };
    let req_str = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n{auth_hdr}{ct_hdr}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req_str.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let hdr_end = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let hdr_str = std::str::from_utf8(&raw[..hdr_end]).unwrap();
    let status: u16 = hdr_str
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    (status, raw[hdr_end + 4..].to_vec())
}

#[tokio::test]
async fn test_rest_catalog_fast_batch_409_full_trace_and_invalid_pace() {
    let (cfg, _tmp) = test_config("batch_rest");
    let state = build_state(cfg).await.unwrap();
    let app = build_router(state.clone());

    // 1. Issue demo token
    let tok_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/demo-token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"sub":"coach-home","role":"home"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(tok_resp.status(), StatusCode::OK);
    let body = tok_resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let token = v["token"].as_str().unwrap().to_owned();

    // 2. Check /catalog
    let cat = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/catalog")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cat.status(), StatusCode::OK);

    // 3. Invalid pace_multiplier = 3 -> 400 InvalidPaceMultiplier
    let bad_pace = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/simulations")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"mode":"live","pace_multiplier":3}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bad_pace.status(), StatusCode::BAD_REQUEST);

    // 4. FastBatch -> 409 ArtifactsDisabledInFastBatch on /frames, /events, /replay
    let fast_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/simulations")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"mode":"fast_batch","max_ticks":150}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(fast_resp.status(), StatusCode::CREATED);
    let fb: serde_json::Value =
        serde_json::from_slice(&fast_resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let fast_id = fb["sim_id"].as_str().unwrap().to_owned();

    for ep in ["frames", "events", "replay"] {
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/simulations/{fast_id}/{ep}"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::CONFLICT, "expected 409 for {ep}");
    }
    let bs_fast = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/simulations/{fast_id}/boxscore"))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bs_fast.status(), StatusCode::OK);

    // 5. FullTrace -> 200 /frames (.hframes.zst), /replay (standalone verified!), /events, /boxscore
    let full_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/simulations")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"mode":"full_trace","rule_id":"fiba","home_team_id":"boston_five_out","away_team_id":"mile_high_hub","seed":77}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(full_resp.status(), StatusCode::CREATED);
    let full_json: serde_json::Value =
        serde_json::from_slice(&full_resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let full_id = full_json["sim_id"].as_str().unwrap();

    let frames_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/simulations/{full_id}/frames"))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(frames_resp.status(), StatusCode::OK);
    let zstd_bytes = frames_resp.into_body().collect().await.unwrap().to_bytes();
    let dec_arc = decode_soa_zstd_archive(&zstd_bytes).unwrap();
    assert!(dec_arc.frames.len() > 1_000);

    // Post-game Film-Room debug frames allowed
    let dbg_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/v1/simulations/{full_id}/frames?stream=debug&format=raw"
                ))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(dbg_resp.status(), StatusCode::OK);

    // Standalone offline replay from /replay
    let rep_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/simulations/{full_id}/replay"))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rep_resp.status(), StatusCode::OK);
    let rep_bytes = rep_resp.into_body().collect().await.unwrap().to_bytes();
    let hreplay = HReplayContainer::from_bytes(&rep_bytes).unwrap();
    let outcome = hreplay.replay_standalone().unwrap();
    assert_eq!(
        hex::encode(outcome.final_rolling_hash),
        full_json["final_rolling_hash"].as_str().unwrap()
    );
}

#[tokio::test]
async fn test_live_ws_binary_frames_coach_cmd_pvp_delete_403_and_forfeit() {
    let (cfg, _tmp) = test_config("live_ws");
    let state = build_state(cfg).await.unwrap();
    let app = build_router(state.clone());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let token_home = state
        .auth
        .issue_token(
            "coach-home".to_owned(),
            hoops_server::Role::Home,
            None,
            3600,
        )
        .unwrap();

    // Create Live simulation via HTTP
    let create_body = serde_json::json!({
        "mode": "live",
        "rule_id": "nba",
        "home_team_id": "boston_five_out",
        "away_team_id": "bay_motion_split",
        "seed": 55,
        "pace_multiplier": 5,
    })
    .to_string();
    let (st, raw_create) = http_req(
        addr,
        "POST",
        "/api/v1/simulations",
        Some(&token_home),
        Some(&create_body),
    )
    .await;
    assert_eq!(st, 201);
    let create_v: serde_json::Value = serde_json::from_slice(&raw_create).unwrap();
    let sim_id = create_v["sim_id"].as_str().unwrap().to_owned();

    // Pre-GameOver debug frame & replay MUST return 403 Forbidden
    let (st_dbg, _) = http_req(
        addr,
        "GET",
        &format!("/api/v1/simulations/{sim_id}/frames?stream=debug"),
        Some(&token_home),
        None,
    )
    .await;
    assert_eq!(st_dbg, 403);
    let (st_rep, _) = http_req(
        addr,
        "GET",
        &format!("/api/v1/simulations/{sim_id}/replay"),
        Some(&token_home),
        None,
    )
    .await;
    assert_eq!(st_rep, 403);
    let (st_whatif, _) = http_req(
        addr,
        "POST",
        &format!("/api/v1/simulations/{sim_id}/whatif"),
        Some(&token_home),
        Some(r#"{"branch_tick":0}"#),
    )
    .await;
    assert_eq!(st_whatif, 403);

    // PvP DELETE by Home participant MUST return 403 UseForfeitForLivePvp
    let (st_del, raw_del) = http_req(
        addr,
        "DELETE",
        &format!("/api/v1/simulations/{sim_id}"),
        Some(&token_home),
        None,
    )
    .await;
    assert_eq!(st_del, 403);
    let del_body: serde_json::Value = serde_json::from_slice(&raw_del).unwrap();
    assert_eq!(del_body["error"]["code"], "UseForfeitForLivePvp");

    // Connect WebSocket and verify Hps1Header + PublicFrameV1 + inbound coach command + idempotency
    let ws_url =
        format!("ws://{addr}/api/v1/ws/simulations/{sim_id}?from_tick=0&token={token_home}");
    let (mut ws_stream, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();

    let mut saw_hps1 = false;
    let mut saw_frame = false;
    let mut saw_sync = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline && (!saw_hps1 || !saw_frame || !saw_sync) {
        if let Ok(Some(Ok(msg))) =
            tokio::time::timeout(Duration::from_secs(2), ws_stream.next()).await
        {
            match msg {
                WsMsg::Binary(b) if !b.is_empty() => {
                    if b[0] == 0x01 {
                        saw_hps1 = true;
                    } else if b[0] == 0x02 {
                        assert_eq!(b.len(), 125);
                        saw_frame = true;
                    }
                }
                WsMsg::Text(t) => {
                    if t.contains("session_sync") {
                        saw_sync = true;
                    }
                }
                _ => {}
            }
        }
    }
    assert!(saw_hps1 && saw_frame && saw_sync);

    // Send coach command via WS + replay same client_request_id for AlreadyAccepted
    let cmd_json = r#"{"client_request_id":1,"cmd":"set_pace_and_pressure","pace":75}"#;
    ws_stream.send(WsMsg::Text(cmd_json.into())).await.unwrap();
    ws_stream.send(WsMsg::Text(cmd_json.into())).await.unwrap();

    let mut accepted_cnt = 0;
    let d2 = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < d2 && accepted_cnt < 2 {
        if let Ok(Some(Ok(WsMsg::Text(t)))) =
            tokio::time::timeout(Duration::from_secs(1), ws_stream.next()).await
        {
            if t.contains("command_accepted") {
                accepted_cnt += 1;
            }
        }
    }
    assert_eq!(accepted_cnt, 2);

    let pause_path = format!("/api/v1/simulations/{sim_id}/playback");
    let token_away = state
        .auth
        .issue_token(
            "coach-away".to_owned(),
            hoops_server::Role::Away,
            None,
            3600,
        )
        .unwrap();
    let (status, _) = http_req(
        addr,
        "POST",
        &pause_path,
        Some(&token_away),
        Some(r#"{"paused":true}"#),
    )
    .await;
    assert_eq!(status, 403);

    let handle = state
        .live_handles
        .read()
        .await
        .get(&sim_id)
        .unwrap()
        .clone();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if state.archive_store.get_entry(&sim_id).unwrap().last_tick > 90 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();

    let (status, paused) = http_req(
        addr,
        "POST",
        &pause_path,
        Some(&token_home),
        Some(r#"{"paused":true}"#),
    )
    .await;
    assert_eq!(status, 200);
    let paused: serde_json::Value = serde_json::from_slice(&paused).unwrap();
    assert_eq!(paused["paused"], true);
    let paused_tick = paused["tick"].as_u64().unwrap() as u32;

    let (mut replay_ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut ticks = Vec::new();
        loop {
            match replay_ws.next().await.unwrap().unwrap() {
                WsMsg::Binary(frame) if frame.first() == Some(&0x02) => {
                    ticks.push(u32::from_le_bytes(frame[1..5].try_into().unwrap()));
                }
                WsMsg::Text(text) if text.contains("catch_up_complete") => break,
                WsMsg::Text(text) if text.contains("session_sync") => {
                    let sync: serde_json::Value = serde_json::from_str(&text).unwrap();
                    assert_eq!(sync["playback_paused"], true);
                }
                WsMsg::Close(frame) => panic!("catch-up WebSocket closed: {frame:?}"),
                _ => {}
            }
        }
        assert_eq!(ticks, (0..=paused_tick).collect::<Vec<_>>());
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(
        handle
            .query_state()
            .await
            .unwrap()
            .tail_frames
            .last()
            .unwrap()
            .tick,
        paused_tick
    );

    let (status, resumed) = http_req(
        addr,
        "POST",
        &pause_path,
        Some(&token_home),
        Some(r#"{"paused":false}"#),
    )
    .await;
    assert_eq!(status, 200);
    let resumed: serde_json::Value = serde_json::from_slice(&resumed).unwrap();
    assert_eq!(resumed["paused"], false);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if state.archive_store.get_entry(&sim_id).unwrap().last_tick > paused_tick {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();

    // POST /forfeit cleanly ends the match and unlocks /replay
    let (st_forfeit, _) = http_req(
        addr,
        "POST",
        &format!("/api/v1/simulations/{sim_id}/forfeit"),
        Some(&token_home),
        Some(r#"{"client_request_id":2}"#),
    )
    .await;
    assert_eq!(st_forfeit, 200);

    tokio::time::sleep(Duration::from_millis(250)).await;
    let (st_rep_ok, rep_bytes) = http_req(
        addr,
        "GET",
        &format!("/api/v1/simulations/{sim_id}/replay"),
        Some(&token_home),
        None,
    )
    .await;
    assert_eq!(st_rep_ok, 200);
    let hreplay = HReplayContainer::from_bytes(&rep_bytes).unwrap();
    assert!(matches!(
        hreplay.manifest.applied_commands.last().unwrap().payload,
        hoops_core::CommandPayload::Player(PlayerCoachCommand::ForfeitMatch)
    ));

    let entry = state.archive_store.get_entry(&sim_id).unwrap();
    let debug_uri = entry.published_debug_uri.expect("终场发布调试帧归档");
    let debug_bytes = state.archive_store.read_artifact(&debug_uri).unwrap();
    let header = hoops_output::Hps1Header::from_init_spec(
        &hreplay.manifest.init_spec,
        hoops_output::StreamVisibility::AuthoritativeFull,
    );
    let (frames, debug, events) = hoops_output::decode_raw_stream(
        &debug_bytes, header.roster_count, header.play_dict.len() as u16, header.def_dict.len() as u16,
    ).unwrap();
    assert_eq!(frames.len(), entry.last_tick as usize + 1);
    let debug = debug.unwrap();
    assert!(frames.iter().zip(debug).all(|(p, d)| p.tick == d.tick));
    assert!(matches!(events.last().unwrap().payload, hoops_core::GameRecordEventPayload::GameEnded { .. }));
}

#[tokio::test]
async fn test_live_sealed_chunks_stream_into_final_archives() {
    let (cfg, tmp) = test_config("live_chunks");
    let state = build_state(cfg).await.unwrap();
    let app = build_router(state.clone());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let token_home = state
        .auth
        .issue_token(
            "coach-home".to_owned(),
            hoops_server::Role::Home,
            None,
            3600,
        )
        .unwrap();
    let create_body = serde_json::json!({
        "mode": "live",
        "rule_id": "nba",
        "home_team_id": "boston_five_out",
        "away_team_id": "mile_high_hub",
        "seed": 3,
        "pace_multiplier": 5,
    })
    .to_string();
    let (st, raw_create) = http_req(
        addr,
        "POST",
        "/api/v1/simulations",
        Some(&token_home),
        Some(&create_body),
    )
    .await;
    assert_eq!(st, 201);
    let sim_id = serde_json::from_slice::<serde_json::Value>(&raw_create).unwrap()["sim_id"]
        .as_str()
        .unwrap()
        .to_owned();

    // 越过 600 帧，使公共分块与调试分块各至少封口一次
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            if state.archive_store.get_entry(&sim_id).unwrap().last_tick > 600 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    let chunks_dir = tmp.path().join("archives").join(&sim_id).join("chunks");
    let chunk_files: Vec<String> = std::fs::read_dir(&chunks_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        chunk_files.iter().any(|name| name.contains("_chunk_0_599.")),
        "{chunk_files:?}"
    );
    assert!(
        chunk_files.iter().any(|name| name.contains("_dbg_0_599.")),
        "{chunk_files:?}"
    );

    let (st_forfeit, _) = http_req(
        addr,
        "POST",
        &format!("/api/v1/simulations/{sim_id}/forfeit"),
        Some(&token_home),
        Some(r#"{"client_request_id":1}"#),
    )
    .await;
    assert_eq!(st_forfeit, 200);

    let entry = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let entry = state.archive_store.get_entry(&sim_id).unwrap();
            if matches!(entry.archive_status, ArchiveStatus::ArchiveComplete { .. }) {
                break entry;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();

    let public_bytes = state
        .archive_store
        .read_artifact(entry.published_artifact_uri.as_ref().unwrap())
        .unwrap();
    let public = decode_soa_zstd_archive(&public_bytes).unwrap();
    assert_eq!(public.frames.len(), entry.last_tick as usize + 1);
    assert!(public.frames.iter().enumerate().all(|(i, frame)| frame.tick as usize == i));
    assert!(public.seek_index.len() >= 2, "sealed chunk + tail expected");
    assert_eq!(public.seek_index[0].start_tick, 0);
    assert_eq!(public.seek_index[0].tick_count, 600);

    let replay_bytes = state
        .archive_store
        .read_artifact(entry.published_replay_uri.as_ref().unwrap())
        .unwrap();
    let hreplay = HReplayContainer::from_bytes(&replay_bytes).unwrap();
    let header = hoops_output::Hps1Header::from_init_spec(
        &hreplay.manifest.init_spec,
        hoops_output::StreamVisibility::AuthoritativeFull,
    );
    let debug_bytes = state
        .archive_store
        .read_artifact(entry.published_debug_uri.as_ref().expect("终场发布调试帧归档"))
        .unwrap();
    let (frames, debug, events) = hoops_output::decode_raw_stream(
        &debug_bytes, header.roster_count, header.play_dict.len() as u16, header.def_dict.len() as u16,
    )
    .unwrap();
    assert_eq!(frames.len(), entry.last_tick as usize + 1);
    assert_eq!(public.frames, frames);
    assert_eq!(public.events, events);
    let debug = debug.unwrap();
    assert!(frames.iter().zip(debug).all(|(p, d)| p.tick == d.tick));
    assert!(matches!(events.last().unwrap().payload, hoops_core::GameRecordEventPayload::GameEnded { .. }));

    // 终场之后 Actor 只保留未封口尾部帧与尾部调试帧
    let handle = state
        .live_handles
        .read()
        .await
        .get(&sim_id)
        .unwrap()
        .clone();
    let query = handle.query_state().await.unwrap();
    assert!(query.tail_frames.len() <= 601, "{}", query.tail_frames.len());
}

fn vm_hwm_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let line = status
        .lines()
        .find(|line| line.starts_with("VmHWM:"))
        .unwrap();
    line.split_whitespace().nth(1).unwrap().parse().unwrap()
}

/// `100` 场并发 Live 内存基准，按需运行：
/// `cargo test --release -p hoops-server --test server_integration bench_live -- --ignored --nocapture`
#[tokio::test]
#[ignore = "100 场并发 Live 内存基准，约数分钟"]
async fn bench_live_100_concurrent_memory_footprint() {
    let games: usize = std::env::var("HOOPS_LIVE_MEMORY_GAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let target_tick: u32 = std::env::var("HOOPS_LIVE_MEMORY_TARGET_TICK")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(900);
    let (cfg, tmp) = test_config("live_memory");
    let state = build_state(cfg).await.unwrap();
    let app = build_router(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let token_home = state
        .auth
        .issue_token(
            "coach-home".to_owned(),
            hoops_server::Role::Home,
            None,
            7200,
        )
        .unwrap();
    let vmhwm_before = vm_hwm_kib();

    let mut sim_ids = Vec::with_capacity(games);
    for i in 0..games {
        let body = serde_json::json!({
            "mode": "live",
            "rule_id": if i % 2 == 0 { "nba" } else { "fiba" },
            "home_team_id": "boston_five_out",
            "away_team_id": "mile_high_hub",
            "seed": 1000 + i,
            "pace_multiplier": 5,
        })
        .to_string();
        let (st, raw) = http_req(
            addr,
            "POST",
            "/api/v1/simulations",
            Some(&token_home),
            Some(&body),
        )
        .await;
        assert_eq!(st, 201, "{raw:?}");
        sim_ids.push(
            serde_json::from_slice::<serde_json::Value>(&raw).unwrap()["sim_id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    let vmhwm_created = vm_hwm_kib();

    let started = std::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(900), async {
        loop {
            let pending = sim_ids
                .iter()
                .filter(|sim_id| state.archive_store.get_entry(sim_id).unwrap().last_tick < target_tick)
                .count();
            if pending == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .unwrap();
    let steady_secs = started.elapsed().as_secs();
    let vmhwm_steady = vm_hwm_kib();

    for (i, sim_id) in sim_ids.iter().enumerate() {
        let (st, _) = http_req(
            addr,
            "POST",
            &format!("/api/v1/simulations/{sim_id}/forfeit"),
            Some(&token_home),
            Some(&format!(r#"{{"client_request_id":{}}}"#, i + 1)),
        )
        .await;
        assert_eq!(st, 200);
    }
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let pending = sim_ids
                .iter()
                .filter(|sim_id| {
                    !matches!(
                        state.archive_store.get_entry(sim_id).unwrap().archive_status,
                        ArchiveStatus::ArchiveComplete { .. }
                    )
                })
                .count();
            if pending == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .unwrap();
    let vmhwm_final = vm_hwm_kib();
    println!(
        "games={games} target_tick={target_tick} steady_secs={steady_secs} vmhwm_before_kib={vmhwm_before} vmhwm_created_kib={vmhwm_created} vmhwm_steady_kib={vmhwm_steady} vmhwm_final_kib={vmhwm_final}"
    );
    assert!(
        vmhwm_final < 512 * 1024,
        "VmHWM {} KiB >= 512 MiB",
        vmhwm_final
    );
    drop(tmp);
}

#[tokio::test]
async fn test_wal_fencing_cas_guards_and_byte_quota_pool() {
    let tmp = TempTestDir::new("wal_cas_quota");
    let wal = SharedDurableWalStore::open(tmp.path().join("wal")).unwrap();
    let store = ArchiveStore::new(tmp.path().join("arch")).unwrap();

    // ByteQuotaPool exact reservation + overflow rejection + drop release
    let pool = Arc::new(ByteQuotaPool::new(1024));
    let g1 = pool.try_reserve(600).unwrap();
    assert!(pool.try_reserve(500).is_none());
    let g2 = pool.try_reserve(424).unwrap();
    assert_eq!(pool.used(), 1024);
    drop(g1);
    drop(g2);
    assert_eq!(pool.used(), 0);

    let bundle = ConfigBundle::load_from_dir(&repo_root().join("configs")).unwrap();
    let spec: MatchInitSpecV1 = bundle
        .build_match_init_spec(
            "nba",
            "boston_five_out",
            "mile_high_hub",
            99,
            StateHashMode::PerTickRolling,
        )
        .unwrap();
    assert!(!spec.to_canonical_le_bytes().is_empty());

    store.insert_entry(hoops_server::SimulationDirectoryEntry {
        sim_id: "s1".to_owned(),
        kind: hoops_server::archive_store::SimulationKind::Live,
        owner_user_id: "u1".to_owned(),
        home_participant_id: "*".to_owned(),
        away_participant_id: "*".to_owned(),
        rule_id: "nba".to_owned(),
        home_team_id: "boston_five_out".to_owned(),
        away_team_id: "mile_high_hub".to_owned(),
        pace_multiplier: 1,
        playback_paused: false,
        lease_epoch: 2,
        owner_node_id: "n2".to_owned(),
        lifecycle: hoops_server::archive_store::MatchLifecycleState::Running,
        abort_reason: None,
        archive_status: ArchiveStatus::ArchiveLiveHealthy,
        published_artifact_uri: None,
        published_replay_uri: None,
        published_debug_uri: None,
        score_home: 0,
        score_away: 0,
        quarter: 1,
        game_clock_ds: 7200,
        game_phase: 0,
        last_tick: 0,
        created_wall_ms: 1,
    });

    assert!(store
        .cas_publish_complete("s1", 1, "a".into(), "r".into(), None)
        .is_err());

    let c1 = store
        .cas_publish_complete(
            "s1",
            2,
            "s1/epoch_2_gen_0.hframes.zst".into(),
            "s1/epoch_2.hreplay".into(),
            None,
        )
        .unwrap();
    assert_eq!(c1, CasOutcome::Updated);

    let c2 = store
        .cas_publish_complete(
            "s1",
            2,
            "s1/epoch_2_gen_0.hframes.zst".into(),
            "s1/epoch_2.hreplay".into(),
            None,
        )
        .unwrap();
    assert_eq!(c2, CasOutcome::AlreadyPublishedSameUri);

    let c3 = store
        .cas_set_archive_status(
            "s1",
            2,
            ArchiveStatus::ArchiveFailed {
                reason: "late timeout".into(),
            },
        )
        .unwrap();
    assert!(matches!(c3, CasOutcome::AlreadyCompleted { .. }));

    assert!(!store.safe_delete_unpublished_candidate("s1", "s1/epoch_2.hreplay"));
    let _ = wal;
}
