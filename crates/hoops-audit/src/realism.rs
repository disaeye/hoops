use hoops_core::{
    step_with_batch_ai, GameEndReason, GamePhase, GameRecordEventPayload, MatchInitSpecV1, SimulationEngine,
    StepStatus, VecSimSink,
};
use hoops_output::{
    median_f64, min_offensive_pair_distance_m, project_match_analytics, AnalyticsVisibility,
    LiveBoxScoreState,
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealismMetricsReport {
    pub games: usize,
    pub completed_games: usize,
    pub avg_total_ppg: f64,
    pub avg_fga_per_team: f64,
    pub avg_tpa_share: f64,
    pub avg_fta_per_fga: f64,
    pub avg_orb_rate: f64,
    pub avg_tov_rate: f64,
    pub avg_pf_per_team: f64,
    pub pace_per_48: f64,
    pub ortg: f64,
    pub efg_pct: f64,
    pub ft_pct: f64,
    pub ast_pct: f64,
    pub crowding_ratio: f64,
    pub median_min_off_pair_m: f64,
}

pub fn evaluate_realism_batch(
    spec_template: &MatchInitSpecV1,
    games: usize,
    base_seed: u64,
    max_ticks: usize,
) -> RealismMetricsReport {
    #[derive(Default)]
    struct GameSample {
        completed: bool,
        ppg: f64,
        fga_per_team: f64,
        tpa_share: f64,
        fta_per_fga: f64,
        orb_rate: f64,
        tov_rate: f64,
        pf_per_team: f64,
        pace_per_48: f64,
        ortg: f64,
        efg_pct: f64,
        ft_pct: f64,
        ast_pct: f64,
        crowding_ratio: f64,
        median_min_off_pair_m: f64,
    }

    let per_game: Vec<GameSample> = (0..games)
        .into_par_iter()
        .map(|i| {
            let mut spec = spec_template.clone();
            spec.seed = base_seed.wrapping_add(i as u64);
            spec.state_hash_mode = hoops_core::StateHashMode::Disabled;
            let mut sink = VecSimSink::default();
            let mut eng = SimulationEngine::new(&spec, &mut sink).unwrap();
            let mut cmds = Vec::new();
            for _ in 0..max_ticks {
                cmds.clear();
                let st = step_with_batch_ai(&mut eng, &mut sink, &mut cmds);
                if matches!(st, StepStatus::Finished { .. } | StepStatus::Aborted { .. }) {
                    break;
                }
            }
            let mut boxs = LiveBoxScoreState::from_init_spec(&spec);
            let mut total_orb = 0i32;
            let mut total_drb = 0i32;
            let mut total_poss_events = 0usize;
            let mut live_clock_ds: i64 = 0;

            let live_phase = GamePhase::LiveBall as u8;
            let mut pair_dists = Vec::new();
            let mut ei = 0usize;
            for f in &sink.public_frames {
                boxs.on_frame(f);
                if (f.ball_flags & 0x20) != 0 {
                    live_clock_ds += 1;
                }
                if f.game_phase == live_phase && f.possession_side < 2 {
                    if let Some(d) = min_offensive_pair_distance_m(f, f.possession_side as usize) {
                        pair_dists.push(d);
                    }
                }
                while ei < sink.events.len() && sink.events[ei].tick <= f.tick {
                    let ev = &sink.events[ei];
                    boxs.apply_event(ev);
                    match ev.payload {
                        GameRecordEventPayload::PossessionEstablished { trigger_kind, .. }
                            if trigger_kind != 4 =>
                        {
                            total_poss_events += 1;
                        }
                        GameRecordEventPayload::ReboundSecured {
                            is_offensive,
                            is_deadball_team_reb,
                            ..
                        } if is_deadball_team_reb == 0 => {
                            if is_offensive != 0 {
                                total_orb += 1;
                            } else {
                                total_drb += 1;
                            }
                        }
                        GameRecordEventPayload::ClockAdjusted {
                            delta_game_clock_ds_signed,
                            ..
                        } => {
                            live_clock_ds -= delta_game_clock_ds_signed as i64;
                        }
                        _ => {}
                    }
                    ei += 1;
                }
            }
            while ei < sink.events.len() {
                let ev = &sink.events[ei];
                boxs.apply_event(ev);
                ei += 1;
            }

            let analytics = project_match_analytics(
                &spec,
                &sink.public_frames,
                None,
                &sink.events,
                AnalyticsVisibility::PostGameFull,
            );

            let tot_pts = (boxs.home.score_official + boxs.away.score_official) as f64;
            let tot_fgm = (boxs.home.fgm + boxs.away.fgm) as f64;
            let tot_fga = (boxs.home.fga + boxs.away.fga) as f64;
            let tot_tpm = (boxs.home.tpm + boxs.away.tpm) as f64;
            let tot_tpa = (boxs.home.tpa + boxs.away.tpa) as f64;
            let tot_ftm = (boxs.home.ftm + boxs.away.ftm) as f64;
            let tot_fta = (boxs.home.fta + boxs.away.fta) as f64;
            let tot_tov = (boxs.home.tov + boxs.away.tov) as f64;
            let tot_ast = (boxs.home.ast + boxs.away.ast) as f64;
            let tot_pf = (boxs.home.pf + boxs.away.pf) as f64;
            let reb_chances = (total_orb + total_drb).max(1) as f64;
            let poss_denom = (tot_fga + 0.44 * tot_fta + tot_tov).max(total_poss_events as f64).max(1.0);
            let game_minutes = (live_clock_ds.max(0) as f64) / 600.0;
            let avg_poss = 0.5 * (analytics.home_efficiency.possessions + analytics.away_efficiency.possessions);
            let pace_per_48 = if game_minutes > 0.05 {
                48.0 * avg_poss / game_minutes
            } else {
                0.0
            };

            GameSample {
                completed: eng.state.end_reason == Some(GameEndReason::Normal),
                ppg: tot_pts / 2.0,
                fga_per_team: tot_fga / 2.0,
                tpa_share: tot_tpa / tot_fga.max(1.0),
                fta_per_fga: tot_fta / tot_fga.max(1.0),
                orb_rate: (total_orb as f64) / reb_chances,
                tov_rate: tot_tov / poss_denom,
                pf_per_team: tot_pf / 2.0,
                pace_per_48,
                ortg: 0.5 * (analytics.home_efficiency.ortg + analytics.away_efficiency.ortg),
                efg_pct: (tot_fgm + 0.5 * tot_tpm) / tot_fga.max(1.0),
                ft_pct: tot_ftm / tot_fta.max(1.0),
                ast_pct: tot_ast / tot_fgm.max(1.0),
                crowding_ratio: 0.5
                    * (analytics.home_efficiency.crowding_ratio
                        + analytics.away_efficiency.crowding_ratio),
                median_min_off_pair_m: median_f64(&pair_dists),
            }
        })
        .collect();

    let n = per_game.len().max(1) as f64;
    RealismMetricsReport {
        games,
        completed_games: per_game.iter().filter(|x| x.completed).count(),
        avg_total_ppg: per_game.iter().map(|x| x.ppg).sum::<f64>() / n,
        avg_fga_per_team: per_game.iter().map(|x| x.fga_per_team).sum::<f64>() / n,
        avg_tpa_share: per_game.iter().map(|x| x.tpa_share).sum::<f64>() / n,
        avg_fta_per_fga: per_game.iter().map(|x| x.fta_per_fga).sum::<f64>() / n,
        avg_orb_rate: per_game.iter().map(|x| x.orb_rate).sum::<f64>() / n,
        avg_tov_rate: per_game.iter().map(|x| x.tov_rate).sum::<f64>() / n,
        avg_pf_per_team: per_game.iter().map(|x| x.pf_per_team).sum::<f64>() / n,
        pace_per_48: per_game.iter().map(|x| x.pace_per_48).sum::<f64>() / n,
        ortg: per_game.iter().map(|x| x.ortg).sum::<f64>() / n,
        efg_pct: per_game.iter().map(|x| x.efg_pct).sum::<f64>() / n,
        ft_pct: per_game.iter().map(|x| x.ft_pct).sum::<f64>() / n,
        ast_pct: per_game.iter().map(|x| x.ast_pct).sum::<f64>() / n,
        crowding_ratio: per_game.iter().map(|x| x.crowding_ratio).sum::<f64>() / n,
        median_min_off_pair_m: per_game
            .iter()
            .map(|x| x.median_min_off_pair_m)
            .sum::<f64>()
            / n,
    }
}
