use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use hoops_core::{GameRecordEventV1, PublicFrameV1};
use hoops_output::decode_soa_zstd_archive_range;
use serde::Deserialize;
use tokio::sync::{broadcast, oneshot};
use tokio::time::Instant;

use crate::auth::{Claims, ExternalCommandRequest};
use crate::config::{
    CLOSE_FRAME_WRITE_TIMEOUT_MS, MAX_WS_CONNECTIONS, PHASE_A_MAX_ROUNDS, PHASE_A_MAX_WALL_MS,
    PHASE_B_MAX_WALL_MS, WS_HWM_BYTES, WS_HWM_MSGS, WS_LWM_BYTES, WS_LWM_MSGS, WS_QUEUE_MAX_BYTES,
    WS_QUEUE_MAX_MSGS, WS_STALL_TIMEOUT_MS,
};
use crate::error::ApiError;
use crate::live_actor::{
    ActorControlMsg, CatchUpHandoffOutcome, LiveMatchHandle, LiveStreamItem, SessionTransportEvent,
};
use crate::routes::AppState;

/// Binary frame prefix tags over WebSocket:
/// 0x01 = Hps1Header, 0x02 = PublicFrameV1 (124 B), 0x04 = GameRecordEventV1.
pub const WS_TAG_HPS1: u8 = 0x01;
pub const WS_TAG_PUBLIC_FRAME: u8 = 0x02;
pub const WS_TAG_GAME_EVENT: u8 = 0x04;

#[derive(Debug, Deserialize)]
pub struct WsConnectQuery {
    #[serde(default)]
    pub from_tick: u32,
    #[serde(default)]
    pub after_event_seq: u64,
    #[serde(default)]
    pub token: Option<String>,
}

struct WsConnGuard(std::sync::Arc<crate::quota::ServerQuotas>);

impl Drop for WsConnGuard {
    fn drop(&mut self) {
        self.0.ws_connections.fetch_sub(1, Ordering::AcqRel);
    }
}

struct OutgoingItem {
    is_binary: bool,
    payload: Vec<u8>,
}

struct BoundedWsQueue {
    buf: VecDeque<OutgoingItem>,
    total_bytes: usize,
    stall_since: Option<Instant>,
}

impl BoundedWsQueue {
    fn new() -> Self {
        Self {
            buf: VecDeque::with_capacity(WS_QUEUE_MAX_MSGS),
            total_bytes: 0,
            stall_since: None,
        }
    }

    fn push(&mut self, item: OutgoingItem) -> Result<(), (u16, String)> {
        let sz = item.payload.len();
        if self.buf.len() >= WS_QUEUE_MAX_MSGS || self.total_bytes + sz > WS_QUEUE_MAX_BYTES {
            return Err((4008, "SlowConsumer".to_owned()));
        }
        self.total_bytes += sz;
        self.buf.push_back(item);
        if self.stall_since.is_none()
            && (self.buf.len() >= WS_HWM_MSGS || self.total_bytes >= WS_HWM_BYTES)
        {
            self.stall_since = Some(Instant::now());
        }
        Ok(())
    }

    fn pop_front(&mut self) -> Option<OutgoingItem> {
        let item = self.buf.pop_front()?;
        self.total_bytes = self.total_bytes.saturating_sub(item.payload.len());
        if self.buf.len() <= WS_LWM_MSGS && self.total_bytes <= WS_LWM_BYTES {
            self.stall_since = None;
        }
        Some(item)
    }

    fn check_stalled_5s(&self) -> bool {
        self.stall_since
            .is_some_and(|t| t.elapsed() >= Duration::from_millis(WS_STALL_TIMEOUT_MS))
    }
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Path(sim_id): Path<String>,
    Query(q): Query<WsConnectQuery>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    let claims = state
        .auth
        .extract_from_headers_or_query(&headers, q.token.as_deref())?;
    if let Some(ref bound_sim) = claims.sim_id {
        if bound_sim != &sim_id {
            return Err(ApiError::forbidden(
                "SimTokenMismatch",
                "token sim_id mismatch",
            ));
        }
    }

    // Pre-upgrade checks MUST return HTTP status codes (503 / 404), never WS close before upgrade
    let cur = state.quotas.ws_connections.load(Ordering::Acquire);
    if cur >= MAX_WS_CONNECTIONS {
        return Err(ApiError::service_unavailable(
            "WsConnectionLimitReached",
            "max 256 WebSocket connections active",
            Some(2),
        ));
    }
    let handle = {
        let guard = state.live_handles.read().await;
        guard.get(&sim_id).cloned()
    }
    .ok_or_else(|| ApiError::not_found("SimulationNotFound", format!("no live match {sim_id}")))?;

    // Acquire atomic connection slot pre-upgrade
    if state
        .quotas
        .ws_connections
        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < MAX_WS_CONNECTIONS).then_some(n + 1)
        })
        .is_err()
    {
        return Err(ApiError::service_unavailable(
            "WsConnectionLimitReached",
            "max 256 WebSocket connections active",
            Some(2),
        ));
    }

    let quotas = state.quotas.clone();
    let archive_store = state.archive_store.clone();
    Ok(ws.on_upgrade(move |socket| async move {
        let _conn_guard = WsConnGuard(quotas);
        run_ws_session(socket, handle, archive_store, claims, q).await;
    }))
}

async fn send_close_out_of_band(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    code: u16,
    reason: String,
) {
    let cf = CloseFrame {
        code,
        reason: reason.into(),
    };
    let _ = tokio::time::timeout(
        Duration::from_millis(CLOSE_FRAME_WRITE_TIMEOUT_MS),
        sink.send(Message::Close(Some(cf))),
    )
    .await;
    let _ = sink.close().await;
}

async fn run_ws_session(
    socket: WebSocket,
    handle: LiveMatchHandle,
    archive_store: std::sync::Arc<crate::archive_store::ArchiveStore>,
    claims: Claims,
    q: WsConnectQuery,
) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // Send 0x01 || Hps1Header first
    let mut hdr_pkt = Vec::with_capacity(1 + 512);
    hdr_pkt.push(WS_TAG_HPS1);
    hdr_pkt.extend_from_slice(&handle.header_pub.to_bytes());
    if ws_tx.send(Message::Binary(hdr_pkt.into())).await.is_err() {
        return;
    }

    // Two-phase bounded reconnect (Phase A disk catch-up <= 4 rounds / 15s, then TransitionPhaseAToB)
    let phase_a_start = Instant::now();
    let mut next_tick = q.from_tick;
    let mut next_ev_seq = q.after_event_seq;
    let mut last_sealed_covered: Option<u32> = None;
    let mut rounds = 0usize;

    let mut pb = loop {
        rounds += 1;
        if rounds > PHASE_A_MAX_ROUNDS
            || phase_a_start.elapsed() >= Duration::from_millis(PHASE_A_MAX_WALL_MS)
        {
            send_close_out_of_band(&mut ws_tx, 4013, "CatchUpBudgetExhausted".to_owned()).await;
            return;
        }

        // Phase A pull any sealed chunks on disk covering next_tick
        if let Ok(snap) = handle.query_state().await {
            for (c_start, c_count, c_path) in &snap.sealed_chunk_paths {
                let c_end = c_start + (*c_count as u32).saturating_sub(1);
                if c_end >= next_tick {
                    if let Ok(bytes) = archive_store.read_artifact(c_path) {
                        if let Ok(dec) = decode_soa_zstd_archive_range(&bytes, 0, u32::MAX) {
                            for f in &dec.frames {
                                if f.tick >= next_tick {
                                    let pkt = encode_frame_ws(f);
                                    if ws_tx.send(Message::Binary(pkt.into())).await.is_err() {
                                        return;
                                    }
                                    next_tick = f.tick + 1;
                                }
                            }
                            for ev in &dec.events {
                                if ev.event_seq > next_ev_seq {
                                    let pkt = encode_event_ws(ev);
                                    if ws_tx.send(Message::Binary(pkt.into())).await.is_err() {
                                        return;
                                    }
                                    next_ev_seq = ev.event_seq;
                                }
                            }
                        }
                    }
                    last_sealed_covered = Some(last_sealed_covered.map_or(c_end, |m| m.max(c_end)));
                }
            }
        }

        let (tx, rx) = oneshot::channel();
        if handle
            .ctrl_tx
            .send(ActorControlMsg::TransitionPhaseAToB {
                from_tick: next_tick,
                after_event_seq: next_ev_seq,
                sealed_covered_through_tick: last_sealed_covered,
                reply: tx,
            })
            .await
            .is_err()
        {
            send_close_out_of_band(&mut ws_tx, 4010, "MatchNodeCrashed".to_owned()).await;
            return;
        }

        match rx.await {
            Ok(CatchUpHandoffOutcome::Ready(pb)) => break pb,
            Ok(CatchUpHandoffOutcome::AwaitSealingFlush(mut wrx)) => {
                let target_tick = next_tick;
                let progress = *wrx.borrow_and_update();
                if progress.max_sealed_disk_tick >= target_tick || !progress.has_inflight_sealing {
                    continue;
                }
                let rem = Duration::from_millis(PHASE_A_MAX_WALL_MS)
                    .saturating_sub(phase_a_start.elapsed());
                let wait_res = tokio::time::timeout(rem, wrx.changed()).await;
                if wait_res.is_err() {
                    send_close_out_of_band(&mut ws_tx, 4013, "CatchUpBudgetExhausted".to_owned())
                        .await;
                    return;
                }
            }
            Ok(CatchUpHandoffOutcome::RetryPhaseA) => continue,
            Ok(CatchUpHandoffOutcome::BudgetExhausted(_)) | Err(_) => {
                send_close_out_of_band(&mut ws_tx, 4013, "CatchUpBudgetExhausted".to_owned()).await;
                return;
            }
        }
    };
    let sync = SessionTransportEvent::SessionSync {
        sim_id: pb.sim_id.clone(),
        lease_epoch: pb.lease_epoch,
        pace_multiplier: pb.pace_multiplier,
        game_phase: pb.game_phase,
        quarter: pb.quarter,
        game_clock_ds: pb.game_clock_ds,
        shot_clock_ds: pb.shot_clock_ds,
        score_home: pb.score_home,
        score_away: pb.score_away,
        last_published_frame_tick: pb.last_published_frame_tick,
        last_published_event_seq: pb.last_published_event_seq,
        boxscore_snapshot: pb.boxscore_snapshot.take(),
        active_challenge_window: pb.active_window.take(),
        playback_paused: pb.playback_paused,
    };
    if ws_tx
        .send(Message::Text(serde_json::to_string(&sync).unwrap().into()))
        .await
        .is_err()
    {
        return;
    }

    if !pb.missing_frame_ranges.is_empty() {
        let hist = SessionTransportEvent::HistoricalFramesUnavailable {
            missing_frame_ranges: pb.missing_frame_ranges.clone(),
        };
        let _ = ws_tx
            .send(Message::Text(serde_json::to_string(&hist).unwrap().into()))
            .await;
    }

    // 快照逐条发送，实时消息队列的容量独立计算。
    let catch_up = async {
        for frame in pb.tail_frames_arc.iter() {
            if frame.tick >= next_tick {
                ws_tx
                    .send(Message::Binary(encode_frame_ws(frame).into()))
                    .await?;
                next_tick = frame.tick + 1;
            }
        }
        for (event, _) in pb.tail_events_arc.iter() {
            if event.event_seq > next_ev_seq {
                ws_tx
                    .send(Message::Binary(encode_event_ws(event).into()))
                    .await?;
                next_ev_seq = event.event_seq;
            }
        }
        let done = SessionTransportEvent::CatchUpComplete {
            from_tick: q.from_tick,
            resumed_at_tick: pb.last_published_frame_tick,
            after_event_seq: next_ev_seq,
        };
        ws_tx
            .send(Message::Text(serde_json::to_string(&done).unwrap().into()))
            .await
    };
    match tokio::time::timeout(Duration::from_millis(PHASE_B_MAX_WALL_MS), catch_up).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) => return,
        Err(_) => {
            send_close_out_of_band(&mut ws_tx, 4013, "CatchUpBudgetExhausted".to_owned()).await;
            return;
        }
    }
    let mut b_rx = pb.rx;
    drop(pb.tail_frames_arc);
    drop(pb.tail_events_arc);
    drop(pb._retain_guard.take());

    let mut queue = BoundedWsQueue::new();
    let mut stall_check = tokio::time::interval(Duration::from_millis(250));
    stall_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        // Flush any queued messages
        while let Some(item) = queue.pop_front() {
            let msg = if item.is_binary {
                Message::Binary(item.payload.into())
            } else {
                Message::Text(String::from_utf8_lossy(&item.payload).into_owned().into())
            };
            if ws_tx.send(msg).await.is_err() {
                return;
            }
        }

        tokio::select! {
            biased;

            _ = stall_check.tick() => {
                if queue.check_stalled_5s() {
                    send_close_out_of_band(&mut ws_tx, 4008, "SlowConsumer".to_owned()).await;
                    return;
                }
            }

            live_res = b_rx.recv() => {
                match live_res {
                    Ok(LiveStreamItem::PublicFrame(f)) => {
                        if f.tick >= next_tick {
                            next_tick = f.tick + 1;
                            if let Err((c, r)) = queue.push(OutgoingItem {
                                is_binary: true,
                                payload: encode_frame_ws(&f),
                            }) {
                                send_close_out_of_band(&mut ws_tx, c, r).await;
                                return;
                            }
                        }
                    }
                    Ok(LiveStreamItem::Event { event, .. }) => {
                        if event.event_seq > next_ev_seq {
                            next_ev_seq = event.event_seq;
                            if let Err((c, r)) = queue.push(OutgoingItem {
                                is_binary: true,
                                payload: encode_event_ws(&event),
                            }) {
                                send_close_out_of_band(&mut ws_tx, c, r).await;
                                return;
                            }
                        }
                    }
                    Ok(LiveStreamItem::Transport(tr)) => {
                        if let Err((c, r)) = queue.push(OutgoingItem {
                            is_binary: false,
                            payload: serde_json::to_vec(&tr).unwrap(),
                        }) {
                            send_close_out_of_band(&mut ws_tx, c, r).await;
                            return;
                        }
                    }
                    Ok(LiveStreamItem::Close { code, reason }) => {
                        send_close_out_of_band(&mut ws_tx, code, reason).await;
                        return;
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        send_close_out_of_band(&mut ws_tx, 4008, "SlowConsumer".to_owned()).await;
                        return;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        let _ = ws_tx.close().await;
                        return;
                    }
                }
            }

            in_msg = ws_rx.next() => {
                match in_msg {
                    Some(Ok(Message::Text(txt))) => {
                        let side = claims.role.command_side();
                        let reply_ev = match serde_json::from_str::<ExternalCommandRequest>(txt.as_str()) {
                            Ok(req) => match side {
                                Some(s) => match req.into_ingress_command(s) {
                                    Ok(ing) => handle.submit_command(ing).await.unwrap_or_else(|e| {
                                        SessionTransportEvent::IngressCommandRejected {
                                            client_request_id: 0,
                                            reason_code: 0xFFFF,
                                            reason: e.message,
                                        }
                                    }),
                                    Err(e) => SessionTransportEvent::IngressCommandRejected {
                                        client_request_id: 0,
                                        reason_code: 1,
                                        reason: e.message,
                                    },
                                },
                                None => SessionTransportEvent::IngressCommandRejected {
                                    client_request_id: req.client_request_id,
                                    reason_code: 0x4003,
                                    reason: "Spectator/Admin role cannot submit coach commands".to_owned(),
                                },
                            },
                            Err(e) => SessionTransportEvent::IngressCommandRejected {
                                client_request_id: 0,
                                reason_code: 0x000D,
                                reason: format!("Invalid command payload: {e}"),
                            },
                        };
                        let _ = queue.push(OutgoingItem {
                            is_binary: false,
                            payload: serde_json::to_vec(&reply_ev).unwrap(),
                        });
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                    _ => {}
                }
            }
        }
    }
}

fn encode_frame_ws(f: &PublicFrameV1) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 124);
    out.push(WS_TAG_PUBLIC_FRAME);
    out.extend_from_slice(&f.to_bytes());
    out
}

fn encode_event_ws(ev: &GameRecordEventV1) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 64);
    out.push(WS_TAG_GAME_EVENT);
    ev.write_le(&mut out);
    out
}
