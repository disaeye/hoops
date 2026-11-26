use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use clap::{Parser, Subcommand};
use hoops_audit::{
    bisect_hash_traces, evaluate_realism_batch, lint_match, run_calibration_check, run_paired_ab,
};
use hoops_config::ConfigBundle;
use hoops_core::*;
use hoops_output::{
    encode_raw_stream, encode_soa_zstd_archive, render_event_zh, HReplayContainer, Hps1Header,
    LiveBoxScoreState, StreamVisibility,
};

#[derive(Parser)]
#[command(name = "hoops-cli", about = "Hoops deterministic basketball engine CLI")]
struct Cli {
    #[arg(long, default_value = "configs")]
    configs_dir: PathBuf,
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a simulation and export frames/events/.hreplay/summary
    Run {
        #[arg(long, default_value = "nba")]
        rule: String,
        #[arg(long, default_value = "boston_five_out")]
        home: String,
        #[arg(long, default_value = "mile_high_hub")]
        away: String,
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(long)]
        max_ticks: Option<usize>,
        #[arg(long)]
        out_dir: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        hash_chain: bool,
    },
    /// Generate frozen HTR1 hash trace + optional raw MatchInitSpec & commands JSON for WASM parity
    TraceHashes {
        #[arg(long, default_value = "nba")]
        rule: String,
        #[arg(long, default_value = "boston_five_out")]
        home: String,
        #[arg(long, default_value = "mile_high_hub")]
        away: String,
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(long, default_value_t = 200)]
        ticks: usize,
        #[arg(long)]
        out_htr: PathBuf,
        #[arg(long)]
        out_spec: Option<PathBuf>,
    },
    /// Binary-search two HTR1 files for first diverged tick
    Bisect {
        #[arg(long)]
        a: PathBuf,
        #[arg(long)]
        b: PathBuf,
    },
    /// Run Layer-1 physics/invariant linter
    Lint {
        #[arg(long, default_value = "nba")]
        rule: String,
        #[arg(long, default_value = "boston_five_out")]
        home: String,
        #[arg(long, default_value = "mile_high_hub")]
        away: String,
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(long)]
        max_ticks: Option<usize>,
    },
    /// Run Layer-2 paired A/B or Layer-3 realism batch audit
    Audit {
        #[arg(long, default_value = "nba")]
        rule: String,
        #[arg(long, default_value = "boston_five_out")]
        home: String,
        #[arg(long, default_value = "mile_high_hub")]
        away: String,
        #[arg(long, default_value_t = 4)]
        games: usize,
        #[arg(long, default_value_t = 11)]
        exp_id: u64,
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(long, value_parser = clap::value_parser!(u8).range(0..=100))]
        variant_home_pace: Option<u8>,
    },
    /// Run holdout calibration check
    Calibrate {
        #[arg(long, default_value = "nba")]
        rule: String,
        #[arg(long, default_value = "boston_five_out")]
        home: String,
        #[arg(long, default_value = "mile_high_hub")]
        away: String,
        #[arg(long, default_value_t = 4)]
        games: usize,
        #[arg(long, default_value_t = 11)]
        exp_id: u64,
        #[arg(long, default_value_t = 42)]
        seed: u64,
    },
    /// Throughput benchmark (headless vs FullTrace)
    Bench {
        #[arg(long, default_value = "nba")]
        rule: String,
        #[arg(long, default_value_t = 5000)]
        ticks: usize,
    },
}

fn main() {
    let cli = Cli::parse();
    match cli.cmd {
        Commands::Bisect { a, b } => {
            let ba = fs::read(&a).expect("failed to read -a HTR1");
            let bb = fs::read(&b).expect("failed to read -b HTR1");
            let ha = HashTraceV1::from_bytes(&ba).expect("invalid -a HTR1");
            let hb = HashTraceV1::from_bytes(&bb).expect("invalid -b HTR1");
            let rep = bisect_hash_traces(&ha, &hb);
            println!("{}", serde_json::to_string_pretty(&rep).unwrap());
            if rep.diverged {
                std::process::exit(2);
            }
        }
        Commands::Run {
            rule,
            home,
            away,
            seed,
            max_ticks,
            out_dir,
            hash_chain,
        } => {
            let bundle = ConfigBundle::load_from_dir(&cli.configs_dir).expect("load configs");
            let mode = if hash_chain {
                StateHashMode::PerTickRolling
            } else {
                StateHashMode::Disabled
            };
            let spec = bundle
                .build_match_init_spec(&rule, &home, &away, seed, mode)
                .expect("build init spec");
            let mut sink = VecSimSink::default();
            let mut eng = SimulationEngine::new(&spec, &mut sink).expect("init engine");
            let mut cmds = Vec::new();
            let limit = max_ticks.unwrap_or(50_000);
            for _ in 0..limit {
                let st = step_with_batch_ai(&mut eng, &mut sink, &mut cmds);
                if matches!(st, StepStatus::Finished { .. } | StepStatus::Aborted { .. }) {
                    break;
                }
            }

            let hdr = Hps1Header::from_init_spec(&spec, StreamVisibility::PublicOnly);
            let mut boxs = LiveBoxScoreState::from_init_spec(&spec);
            let mut ei = 0usize;
            for f in &sink.public_frames {
                boxs.on_frame(f);
                while ei < sink.events.len() && sink.events[ei].tick <= f.tick {
                    boxs.apply_event(&sink.events[ei]);
                    ei += 1;
                }
            }
            while ei < sink.events.len() {
                boxs.apply_event(&sink.events[ei]);
                ei += 1;
            }
            let pbp_lines: Vec<String> = sink
                .events
                .iter()
                .map(|ev| render_event_zh(&hdr, ev))
                .collect();
            if let Some(dir) = out_dir {
                fs::create_dir_all(&dir).expect("mkdir out_dir");
                fs::write(dir.join("header.hps1"), hdr.to_bytes()).unwrap();
                fs::write(
                    dir.join("stream.hrw1"),
                    encode_raw_stream(&sink.public_frames, None, &sink.events),
                )
                .unwrap();
                let zstd_arc =
                    encode_soa_zstd_archive(&hdr, &sink.public_frames, &sink.events, 3).unwrap();
                fs::write(dir.join("frames.hframes.zst"), zstd_arc).unwrap();
                let hreplay = HReplayContainer {
                    manifest: ReplayManifest {
                        init_spec: spec,
                        applied_commands: cmds,
                        verification: None,
                    },
                    hash_trace: None,
                };
                fs::write(dir.join("match.hreplay"), hreplay.to_bytes()).unwrap();
                fs::write(
                    dir.join("boxscore.json"),
                    serde_json::to_string_pretty(&boxs).unwrap(),
                )
                .unwrap();
                fs::write(dir.join("pbp_zh.txt"), pbp_lines.join("\n")).unwrap();
            }

            let summary = serde_json::json!({
                "ticks": sink.public_frames.len(),
                "events": sink.events.len(),
                "score_home": eng.state.team_states[0].score_official_override.unwrap_or(eng.state.team_states[0].score_on_court),
                "score_away": eng.state.team_states[1].score_official_override.unwrap_or(eng.state.team_states[1].score_on_court),
                "final_rolling_hash": hex::encode(eng.last_hashes.2),
            });
            println!("{}", serde_json::to_string_pretty(&summary).unwrap());
        }
        Commands::TraceHashes {
            rule,
            home,
            away,
            seed,
            ticks,
            out_htr,
            out_spec,
        } => {
            let bundle = ConfigBundle::load_from_dir(&cli.configs_dir).expect("load configs");
            let spec = bundle
                .build_match_init_spec(&rule, &home, &away, seed, StateHashMode::PerTickRolling)
                .expect("build spec");
            let spec_bytes = spec.to_canonical_le_bytes();
            if let Some(sp) = out_spec {
                fs::write(&sp, &spec_bytes).expect("write spec");
            }
            let mut sink = VecSimSink::default();
            let mut eng = SimulationEngine::new(&spec, &mut sink).expect("init engine");
            let mut cmds = Vec::new();
            for _ in 0..ticks {
                let st = step_with_batch_ai(&mut eng, &mut sink, &mut cmds);
                if matches!(st, StepStatus::Finished { .. } | StepStatus::Aborted { .. }) {
                    break;
                }
            }
            let cmds_bytes = cmds.to_canonical_le_bytes();
            let htr = HashTraceV1 {
                format_version: HTR1_VERSION,
                start_tick: 0,
                tick_count: sink.hashes.len() as u32,
                init_spec_sha256: sha256_bytes(&spec_bytes),
                applied_commands_sha256: sha256_bytes(&cmds_bytes),
                records: sink
                    .hashes
                    .iter()
                    .map(|&(t, sh, eh, rh)| HashTraceRecordV1 {
                        tick: t,
                        state_hash: sh,
                        event_hash: eh,
                        rolling_hash: rh,
                    })
                    .collect(),
            };
            fs::write(&out_htr, htr.to_bytes()).expect("write htr");
            println!(
                "{{\"records\":{},\"final_rolling_hash\":\"{}\"}}",
                htr.records.len(),
                hex::encode(eng.last_hashes.2)
            );
        }
        Commands::Lint {
            rule,
            home,
            away,
            seed,
            max_ticks,
        } => {
            let bundle = ConfigBundle::load_from_dir(&cli.configs_dir).expect("load configs");
            let spec = bundle
                .build_match_init_spec(&rule, &home, &away, seed, StateHashMode::Disabled)
                .expect("build spec");
            let rep = lint_match(&spec, max_ticks).expect("lint");
            println!("{}", serde_json::to_string_pretty(&rep).unwrap());
            if !rep.passed {
                std::process::exit(1);
            }
        }
        Commands::Audit {
            rule,
            home,
            away,
            games,
            exp_id,
            seed,
            variant_home_pace,
        } => {
            let bundle = ConfigBundle::load_from_dir(&cli.configs_dir).expect("load configs");
            let spec_a = bundle
                .build_match_init_spec(&rule, &home, &away, seed, StateHashMode::Disabled)
                .expect("build spec");
            let mut spec_b = spec_a.clone();
            let baseline_pace = spec_a.home_team.tactics.pace;
            spec_b.home_team.tactics.pace = variant_home_pace.unwrap_or_else(|| {
                if baseline_pace <= 94 { baseline_pace + 6 } else { baseline_pace - 6 }
            });
            let ab = run_paired_ab(&spec_a, &spec_b, games, exp_id, MAX_SIM_TICKS as usize);
            let realism = evaluate_realism_batch(&spec_a, games, seed, MAX_SIM_TICKS as usize);
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "baseline_home_pace": baseline_pace,
                    "variant_home_pace": spec_b.home_team.tactics.pace,
                    "ab": ab,
                    "realism": realism,
                }))
                .unwrap()
            );
        }
        Commands::Calibrate {
            rule,
            home,
            away,
            games,
            exp_id,
            seed,
        } => {
            let bundle = ConfigBundle::load_from_dir(&cli.configs_dir).expect("load configs");
            let spec = bundle
                .build_match_init_spec(&rule, &home, &away, seed, StateHashMode::Disabled)
                .expect("build spec");
            let rep = run_calibration_check(&spec, exp_id, games, MAX_SIM_TICKS as usize);
            println!("{}", serde_json::to_string_pretty(&rep).unwrap());
            if !rep.within_targets {
                std::process::exit(1);
            }
        }
        Commands::Bench { rule, ticks } => {
            let bundle = ConfigBundle::load_from_dir(&cli.configs_dir).expect("load configs");
            let spec = bundle
                .build_match_init_spec(
                    &rule,
                    "boston_five_out",
                    "mile_high_hub",
                    1,
                    StateHashMode::Disabled,
                )
                .expect("build spec");
            let t0 = Instant::now();
            let mut sink = NullSimSink;
            let mut eng = SimulationEngine::new(&spec, &mut sink).expect("init engine");
            let mut cmds = Vec::new();
            let mut done = 0usize;
            for _ in 0..ticks {
                let st = step_with_batch_ai(&mut eng, &mut sink, &mut cmds);
                done += 1;
                if matches!(st, StepStatus::Finished { .. } | StepStatus::Aborted { .. }) {
                    break;
                }
            }
            let elapsed = t0.elapsed().as_secs_f64();
            let tps = (done as f64) / elapsed.max(1.0e-6);
            println!("{{\"ticks\":{done},\"elapsed_s\":{elapsed:.4},\"ticks_per_sec\":{tps:.1}}}");
        }
    }
}
