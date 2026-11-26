use axum::http::Request;
use axum::body::Body;
use hoops_core::{
    AdvanceOption, ChallengeTarget, CommandPayload, CommandPhase, CommandSide, GameplanId,
    IngressCommand, InlinePlayerList, PatchField, PlayId, PlayerCoachCommand, PnrRuleId,
};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Home,
    Away,
    Spectator,
    ServiceAdmin,
}

impl Role {
    pub fn command_side(self) -> Option<CommandSide> {
        match self {
            Self::Home => Some(CommandSide::Home),
            Self::Away => Some(CommandSide::Away),
            Self::Spectator | Self::ServiceAdmin => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub role: Role,
    #[serde(default)]
    pub sim_id: Option<String>,
    pub exp: usize,
}

#[derive(Clone)]
pub struct JwtAuth {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
}

impl JwtAuth {
    pub fn new(secret: &str) -> Self {
        Self {
            encoding_key: EncodingKey::from_secret(secret.as_bytes()),
            decoding_key: DecodingKey::from_secret(secret.as_bytes()),
        }
    }

    pub fn issue_token(
        &self,
        sub: String,
        role: Role,
        sim_id: Option<String>,
        ttl_secs: usize,
    ) -> Result<String, ApiError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as usize;
        let claims = Claims {
            sub,
            role,
            sim_id,
            exp: now.saturating_add(ttl_secs),
        };
        encode(&Header::new(Algorithm::HS256), &claims, &self.encoding_key)
            .map_err(|e| ApiError::internal(format!("jwt encode failed: {e}")))
    }

    pub fn verify_token(&self, token: &str) -> Result<Claims, ApiError> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = true;
        decode::<Claims>(token, &self.decoding_key, &validation)
            .map(|data| data.claims)
            .map_err(|e| ApiError::forbidden("InvalidAuthToken", format!("invalid jwt: {e}")))
    }

    pub fn extract_from_headers_or_query(
        &self,
        headers: &axum::http::HeaderMap,
        query_token: Option<&str>,
    ) -> Result<Claims, ApiError> {
        if let Some(auth) = headers.get(axum::http::header::AUTHORIZATION) {
            if let Ok(s) = auth.to_str() {
                if let Some(tok) = s.strip_prefix("Bearer ").or_else(|| s.strip_prefix("bearer ")) {
                    return self.verify_token(tok.trim());
                }
            }
        }
        if let Some(tok) = query_token {
            if !tok.is_empty() {
                return self.verify_token(tok);
            }
        }
        Err(ApiError::forbidden(
            "MissingAuthToken",
            "Authorization Bearer token or ?token=... is required",
        ))
    }
}

pub fn extract_bearer(req: &Request<Body>) -> Option<&str> {
    req.headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer ").or_else(|| s.strip_prefix("bearer ")))
        .map(str::trim)
}

/// Strictly external command DTO (§11.3: physically forbids deserializing `SystemCommand`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum ExternalCoachCommand {
    CallTimeout {
        #[serde(default)]
        queue_if_live_disallowed: bool,
        #[serde(default)]
        advance: Option<u8>,
    },
    ChallengeCall {
        #[serde(default)]
        target_kind: u8,
        #[serde(default)]
        ruling_id: Option<u64>,
        #[serde(default)]
        target_event_seq: Option<u64>,
        #[serde(default)]
        expected_window_epoch: Option<u32>,
    },
    WithdrawChallenge {
        #[serde(default)]
        target_kind: u8,
        #[serde(default)]
        ruling_id: Option<u64>,
        #[serde(default)]
        target_event_seq: Option<u64>,
        expected_request_seq: u32,
        #[serde(default)]
        expected_window_epoch: Option<u32>,
    },
    Substitute {
        out_player: u8,
        in_player: u8,
    },
    SetOffensivePlay {
        play_id: String,
        #[serde(default)]
        force_immediate: bool,
    },
    SetDefensiveGameplan {
        #[serde(default)]
        gameplan_id: Option<String>,
        #[serde(default)]
        protect_targets: Option<Vec<u8>>,
        #[serde(default)]
        clear_protect_targets: bool,
        #[serde(default)]
        pnr_rule_override: Option<String>,
        #[serde(default)]
        clear_pnr_rule_override: bool,
        #[serde(default)]
        matchup_override: Option<(u8, u8)>,
        #[serde(default)]
        clear_matchup_override: bool,
    },
    SetPaceAndPressure {
        #[serde(default)]
        pace: Option<u8>,
        #[serde(default)]
        full_court_press: Option<bool>,
    },
    SetTimeoutAdvance {
        advance: u8,
        #[serde(default)]
        expected_window_epoch: Option<u32>,
    },
    ForfeitMatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalCommandRequest {
    pub client_request_id: u64,
    #[serde(default)]
    pub apply_at_tick: Option<u32>,
    #[serde(default)]
    pub phase: Option<u8>,
    #[serde(flatten)]
    pub command: ExternalCoachCommand,
}

impl ExternalCommandRequest {
    pub fn into_ingress_command(self, side: CommandSide) -> Result<IngressCommand, ApiError> {
        if !matches!(side, CommandSide::Home | CommandSide::Away) {
            return Err(ApiError::forbidden(
                "RoleCannotSubmitCommand",
                "only Home or Away participants can submit coach commands",
            ));
        }
        let phase = match self.phase {
            None | Some(0) => CommandPhase::PreTick,
            Some(1) => CommandPhase::PostWhistle,
            Some(other) => {
                return Err(ApiError::bad_request(
                    "InvalidCommandPhase",
                    format!("invalid phase {other}"),
                ))
            }
        };

        let p_cmd = match self.command {
            ExternalCoachCommand::CallTimeout {
                queue_if_live_disallowed,
                advance,
            } => {
                let adv = AdvanceOption::from_u8(advance.unwrap_or(0)).ok_or_else(|| {
                    ApiError::bad_request("InvalidAdvanceOption", "advance must be 0..=2")
                })?;
                PlayerCoachCommand::CallTimeout {
                    queue_if_live_disallowed,
                    advance: adv,
                }
            }
            ExternalCoachCommand::ChallengeCall {
                target_kind,
                ruling_id,
                target_event_seq,
                expected_window_epoch,
            } => {
                let target = if target_kind == 1 {
                    ChallengeTarget::RecentScoringOrClockEvent {
                        event_seq: target_event_seq.unwrap_or(0),
                    }
                } else {
                    ChallengeTarget::Ruling(ruling_id.unwrap_or(0))
                };
                PlayerCoachCommand::ChallengeCall {
                    target,
                    expected_window_epoch,
                }
            }
            ExternalCoachCommand::WithdrawChallenge {
                target_kind,
                ruling_id,
                target_event_seq,
                expected_request_seq,
                expected_window_epoch,
            } => {
                let target = if target_kind == 1 {
                    ChallengeTarget::RecentScoringOrClockEvent {
                        event_seq: target_event_seq.unwrap_or(0),
                    }
                } else {
                    ChallengeTarget::Ruling(ruling_id.unwrap_or(0))
                };
                PlayerCoachCommand::WithdrawChallenge {
                    target,
                    expected_request_seq,
                    expected_window_epoch,
                }
            }
            ExternalCoachCommand::Substitute {
                out_player,
                in_player,
            } => PlayerCoachCommand::Substitute {
                out_player,
                in_player,
            },
            ExternalCoachCommand::SetOffensivePlay {
                play_id,
                force_immediate,
            } => {
                let pid = PlayId::new(&play_id).map_err(|_| {
                    ApiError::bad_request("InvalidPlayId", "play_id length or characters invalid")
                })?;
                PlayerCoachCommand::SetOffensivePlay {
                    play_id: pid,
                    force_immediate,
                }
            }
            ExternalCoachCommand::SetDefensiveGameplan {
                gameplan_id,
                protect_targets,
                clear_protect_targets,
                pnr_rule_override,
                clear_pnr_rule_override,
                matchup_override,
                clear_matchup_override,
            } => {
                let gid = match gameplan_id {
                    Some(ref s) => PatchField::Set(GameplanId::new(s).map_err(|_| {
                        ApiError::bad_request("InvalidGameplanId", "invalid gameplan_id")
                    })?),
                    None => PatchField::Keep,
                };
                let pt = if clear_protect_targets {
                    PatchField::Clear
                } else if let Some(list) = protect_targets {
                    let il = InlinePlayerList::<5>::from_slice(&list).map_err(|_| {
                        ApiError::bad_request("InvalidProtectTargets", "max 5 protect targets")
                    })?;
                    PatchField::Set(il)
                } else {
                    PatchField::Keep
                };
                let pr = if clear_pnr_rule_override {
                    PatchField::Clear
                } else if let Some(ref r) = pnr_rule_override {
                    PatchField::Set(PnrRuleId::new(r).map_err(|_| {
                        ApiError::bad_request("InvalidPnrRuleId", "invalid pnr_rule_override")
                    })?)
                } else {
                    PatchField::Keep
                };
                let mo = if clear_matchup_override {
                    PatchField::Clear
                } else if let Some(pair) = matchup_override {
                    PatchField::Set(pair)
                } else {
                    PatchField::Keep
                };
                PlayerCoachCommand::SetDefensiveGameplan {
                    gameplan_id: gid,
                    protect_targets: pt,
                    pnr_rule_override: pr,
                    matchup_override: mo,
                }
            }
            ExternalCoachCommand::SetPaceAndPressure {
                pace,
                full_court_press,
            } => PlayerCoachCommand::SetPaceAndPressure {
                pace: pace.map_or(PatchField::Keep, PatchField::Set),
                full_court_press: full_court_press.map_or(PatchField::Keep, PatchField::Set),
            },
            ExternalCoachCommand::SetTimeoutAdvance {
                advance,
                expected_window_epoch,
            } => {
                let adv = AdvanceOption::from_u8(advance).ok_or_else(|| {
                    ApiError::bad_request("InvalidAdvanceOption", "advance must be 0..=2")
                })?;
                PlayerCoachCommand::SetTimeoutAdvance {
                    advance: adv,
                    expected_window_epoch,
                }
            }
            ExternalCoachCommand::ForfeitMatch => PlayerCoachCommand::ForfeitMatch,
        };

        Ok(IngressCommand {
            client_request_id: self.client_request_id,
            apply_at_tick: self.apply_at_tick,
            phase,
            side,
            payload: CommandPayload::Player(p_cmd),
        })
    }
}
