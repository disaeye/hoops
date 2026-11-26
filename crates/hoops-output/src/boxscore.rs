use std::collections::BTreeMap;

use hoops_core::{GameRecordEventPayload, GameRecordEventV1, MatchInitSpecV1, PublicFrameV1};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlayerBoxLine {
    pub roster_idx: u8,
    pub player_id: String,
    pub display_name: String,
    pub jersey_number_code: u8,
    pub court_time_ds: u32,
    pub pts: i32,
    pub fgm: i32,
    pub fga: i32,
    pub tpm: i32,
    pub tpa: i32,
    pub ftm: i32,
    pub fta: i32,
    pub oreb: i32,
    pub dreb: i32,
    pub ast: i32,
    pub stl: i32,
    pub blk: i32,
    pub tov: i32,
    pub pf: i32,
    pub fouled_out: bool,
    pub ejected: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamBoxSummary {
    pub team_id: String,
    pub score_on_court: u16,
    pub score_official: u16,
    pub fgm: i32,
    pub fga: i32,
    pub tpm: i32,
    pub tpa: i32,
    pub ftm: i32,
    pub fta: i32,
    pub oreb: i32,
    pub dreb: i32,
    pub ast: i32,
    pub stl: i32,
    pub blk: i32,
    pub tov: i32,
    pub pf: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveBoxScoreState {
    pub quarter: u8,
    pub game_clock_ds: u16,
    pub last_event_seq: u64,
    pub game_ended: bool,
    pub winner_side: Option<u8>,
    pub end_reason: Option<u8>,
    pub home: TeamBoxSummary,
    pub away: TeamBoxSummary,
    pub players: Vec<PlayerBoxLine>,
    #[serde(skip)]
    pub pending_shot_values: BTreeMap<u32, u8>,
    #[serde(skip)]
    pub last_frame_tick: Option<u32>,
}

impl Default for LiveBoxScoreState {
    fn default() -> Self {
        Self::from_roster_names(String::new(), String::new(), &[], &[])
    }
}

impl LiveBoxScoreState {
    pub fn from_init_spec(spec: &MatchInitSpecV1) -> Self {
        let home_names: Vec<_> = spec
            .home_team
            .players
            .iter()
            .enumerate()
            .take(15)
            .map(|(i, p)| {
                (
                    i as u8,
                    p.player_id.clone(),
                    p.display_name.clone(),
                    p.jersey_number_code,
                )
            })
            .collect();
        let away_names: Vec<_> = spec
            .away_team
            .players
            .iter()
            .enumerate()
            .take(15)
            .map(|(i, p)| {
                (
                    (15 + i) as u8,
                    p.player_id.clone(),
                    p.display_name.clone(),
                    p.jersey_number_code,
                )
            })
            .collect();
        let mut s = Self::from_roster_names(
            spec.home_team.team_id.clone(),
            spec.away_team.team_id.clone(),
            &home_names,
            &away_names,
        );
        s.game_clock_ds = spec.ruleset.clocks.quarter_sec.saturating_mul(10);
        s
    }

    pub fn from_roster_names(
        home_team_id: String,
        away_team_id: String,
        home_players: &[(u8, String, String, u8)],
        away_players: &[(u8, String, String, u8)],
    ) -> Self {
        let mut players = vec![PlayerBoxLine::default(); 30];
        for (idx, line) in players.iter_mut().enumerate() {
            line.roster_idx = idx as u8;
        }
        for &(r, ref id, ref name, jersey) in home_players.iter().chain(away_players.iter()) {
            if (r as usize) < 30 {
                players[r as usize].player_id = id.clone();
                players[r as usize].display_name = name.clone();
                players[r as usize].jersey_number_code = jersey;
            }
        }
        Self {
            quarter: 1,
            game_clock_ds: 7200,
            last_event_seq: 0,
            game_ended: false,
            winner_side: None,
            end_reason: None,
            home: TeamBoxSummary {
                team_id: home_team_id,
                ..TeamBoxSummary::default()
            },
            away: TeamBoxSummary {
                team_id: away_team_id,
                ..TeamBoxSummary::default()
            },
            players,
            pending_shot_values: BTreeMap::new(),
            last_frame_tick: None,
        }
    }

    pub fn on_frame(&mut self, frame: &PublicFrameV1) {
        if self.last_frame_tick.is_some_and(|t| frame.tick <= t) {
            return;
        }
        self.last_frame_tick = Some(frame.tick);
        self.quarter = frame.quarter;
        self.game_clock_ds = frame.game_clock_ds;
        if (frame.ball_flags & 0x20) != 0 {
            for slot in &frame.players {
                let idx = slot.roster_idx as usize;
                if idx < 30 {
                    self.players[idx].court_time_ds += 1;
                }
            }
        }
    }

    fn team_mut(&mut self, roster_idx: u8) -> &mut TeamBoxSummary {
        if (roster_idx as usize) < 15 {
            &mut self.home
        } else {
            &mut self.away
        }
    }

    pub fn apply_event(&mut self, ev: &GameRecordEventV1) {
        if ev.event_seq <= self.last_event_seq {
            return;
        }
        self.quarter = ev.quarter;
        self.game_clock_ds = ev.game_clock_ds;
        self.last_event_seq = ev.event_seq;
        match &ev.payload {
            GameRecordEventPayload::ShotReleased {
                shot_id,
                shot_value,
                ..
            } => {
                self.pending_shot_values.insert(*shot_id, *shot_value);
            }
            GameRecordEventPayload::ShotResolved {
                shot_id,
                shooter_roster_idx,
                points_awarded,
                is_blocked,
                blocker_roster_idx,
                assist_roster_idx,
                foul_on_shot,
                ..
            } => {
                let s = *shooter_roster_idx as usize;
                let pts = *points_awarded as i32;
                let made = pts > 0;
                let count_fga = made || *foul_on_shot == 0;
                let is_three = self
                    .pending_shot_values
                    .remove(shot_id)
                    .map(|v| v == 3)
                    .unwrap_or(pts == 3);
                if s < 30 {
                    let p = &mut self.players[s];
                    p.pts += pts;
                    if count_fga {
                        p.fga += 1;
                    }
                    if made {
                        p.fgm += 1;
                    }
                    if is_three && count_fga {
                        p.tpa += 1;
                    }
                    if is_three && made {
                        p.tpm += 1;
                    }
                    let tm = self.team_mut(*shooter_roster_idx);
                    tm.score_on_court = (tm.score_on_court as i32 + pts).max(0) as u16;
                    tm.score_official = tm.score_on_court;
                    if count_fga {
                        tm.fga += 1;
                    }
                    if made {
                        tm.fgm += 1;
                    }
                    if is_three && count_fga {
                        tm.tpa += 1;
                    }
                    if is_three && made {
                        tm.tpm += 1;
                    }
                }
                let a = *assist_roster_idx as usize;
                if made && a < 30 {
                    self.players[a].ast += 1;
                    self.team_mut(*assist_roster_idx).ast += 1;
                }
                if *is_blocked != 0 && (*blocker_roster_idx as usize) < 30 {
                    let b = *blocker_roster_idx;
                    self.players[b as usize].blk += 1;
                    self.team_mut(b).blk += 1;
                }
            }
            GameRecordEventPayload::FreeThrowResolved {
                shooter_roster_idx,
                made,
                ft_flags,
                ..
            } => {
                if (ft_flags & 0x01) != 0 && (*shooter_roster_idx as usize) < 30 {
                    let s = *shooter_roster_idx;
                    self.players[s as usize].fta += 1;
                    self.team_mut(s).fta += 1;
                    if *made != 0 {
                        self.players[s as usize].ftm += 1;
                        self.players[s as usize].pts += 1;
                        let tm = self.team_mut(s);
                        tm.ftm += 1;
                        tm.score_on_court += 1;
                        tm.score_official = tm.score_on_court;
                    }
                }
            }
            GameRecordEventPayload::ReboundSecured {
                rebounder_roster_idx,
                is_offensive,
                is_deadball_team_reb,
                ..
            } => {
                if *is_deadball_team_reb == 0 && (*rebounder_roster_idx as usize) < 30 {
                    let r = *rebounder_roster_idx;
                    if *is_offensive != 0 {
                        self.players[r as usize].oreb += 1;
                        self.team_mut(r).oreb += 1;
                    } else {
                        self.players[r as usize].dreb += 1;
                        self.team_mut(r).dreb += 1;
                    }
                }
            }
            GameRecordEventPayload::TurnoverCommitted {
                offender_roster_idx,
                stealer_roster_idx,
                is_live_steal,
                ..
            } => {
                if (*offender_roster_idx as usize) < 30 {
                    let o = *offender_roster_idx;
                    self.players[o as usize].tov += 1;
                    self.team_mut(o).tov += 1;
                }
                if *is_live_steal != 0 && (*stealer_roster_idx as usize) < 30 {
                    let st = *stealer_roster_idx;
                    self.players[st as usize].stl += 1;
                    self.team_mut(st).stl += 1;
                }
            }
            GameRecordEventPayload::FoulAssessedRecord {
                offender_roster_idx,
                ejected_or_fouled_out_flags,
                ..
            } => {
                if (*offender_roster_idx as usize) < 30 {
                    let o = *offender_roster_idx;
                    self.players[o as usize].pf += 1;
                    if (ejected_or_fouled_out_flags & 0x01) != 0 {
                        self.players[o as usize].fouled_out = true;
                    }
                    if (ejected_or_fouled_out_flags & 0x02) != 0 {
                        self.players[o as usize].ejected = true;
                    }
                    self.team_mut(o).pf += 1;
                }
            }
            GameRecordEventPayload::ScoringEventAmended {
                shooter_roster_idx,
                assist_roster_idx,
                team_side,
                pts_delta,
                fga_delta,
                fgm_delta,
                tpa_delta,
                tpm_delta,
                ast_delta,
                blocker_roster_idx,
                blk_delta,
                ..
            } => {
                let s = *shooter_roster_idx as usize;
                if s < 30 {
                    let p = &mut self.players[s];
                    p.pts += *pts_delta as i32;
                    p.fga += *fga_delta as i32;
                    p.fgm += *fgm_delta as i32;
                    p.tpa += *tpa_delta as i32;
                    p.tpm += *tpm_delta as i32;
                }
                if (*assist_roster_idx as usize) < 30 {
                    self.players[*assist_roster_idx as usize].ast += *ast_delta as i32;
                }
                if (*blocker_roster_idx as usize) < 30 {
                    self.players[*blocker_roster_idx as usize].blk += *blk_delta as i32;
                }
                let tm = if *team_side == 0 {
                    &mut self.home
                } else {
                    &mut self.away
                };
                tm.score_on_court = (tm.score_on_court as i32 + *pts_delta as i32).max(0) as u16;
                tm.score_official = tm.score_on_court;
                tm.fga += *fga_delta as i32;
                tm.fgm += *fgm_delta as i32;
                tm.tpa += *tpa_delta as i32;
                tm.tpm += *tpm_delta as i32;
                tm.ast += *ast_delta as i32;
                tm.blk += *blk_delta as i32;
            }
            GameRecordEventPayload::SubstitutionApplied {
                out_roster_idx,
                out_player_cum_court_time_ds,
                ..
            } => {
                if (*out_roster_idx as usize) < 30 {
                    self.players[*out_roster_idx as usize].court_time_ds =
                        *out_player_cum_court_time_ds;
                }
            }
            GameRecordEventPayload::ClockAdjusted {
                delta_game_clock_ds_signed,
                affected_slot_mask,
                affected_roster_indices,
                ..
            } => {
                for slot in 0..10 {
                    if (affected_slot_mask & (1 << slot)) != 0 {
                        let r = affected_roster_indices[slot] as usize;
                        if r < 30 {
                            let new_t = (self.players[r].court_time_ds as i64)
                                - (*delta_game_clock_ds_signed as i64);
                            self.players[r].court_time_ds = new_t.max(0) as u32;
                        }
                    }
                }
            }
            GameRecordEventPayload::GameEnded {
                score_home_official,
                score_away_official,
                on_court_score_home,
                on_court_score_away,
                winner_side,
                end_reason,
                final_court_time_ds,
                ..
            } => {
                self.game_ended = true;
                self.winner_side = Some(*winner_side);
                self.end_reason = Some(*end_reason);
                self.home.score_on_court = *on_court_score_home;
                self.away.score_on_court = *on_court_score_away;
                self.home.score_official = *score_home_official;
                self.away.score_official = *score_away_official;
                for i in 0..30 {
                    self.players[i].court_time_ds = final_court_time_ds.0[i];
                }
            }
            _ => {}
        }
    }
}
