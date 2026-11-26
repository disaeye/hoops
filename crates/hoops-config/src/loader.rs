use std::fs;
use std::path::{Path, PathBuf};

use hoops_core::*;
use serde::Deserialize;

use crate::compiler::{
    compile_defense, compile_playbook, compile_team, validate_badges, validate_ruleset,
    validate_tunables,
};
use crate::error::ConfigError;
use crate::hasher::compute_config_hashes;

#[derive(Debug, Deserialize)]
pub struct BadgesFile {
    pub badges: Vec<BadgeDef>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum RawMetricKey {
    Id(u16),
    Name(String),
}

#[derive(Debug, Deserialize)]
pub struct RawSlotWeights {
    pub role_slot: u8,
    pub terms: Vec<(RawMetricKey, f64)>,
}

#[derive(Debug, Deserialize)]
pub struct RawAnchorPos {
    pub anchor_kind: u8,
    pub side_tag: u8,
    pub dx_m: f64,
    pub dy_m: f64,
}

#[derive(Debug, Deserialize)]
pub struct RawPlayPhase {
    pub base_epv: f64,
    pub max_duration_ds: u16,
    pub exit_trigger_kind: u8,
    pub exit_target_role: u8,
    pub exit_fallback_ds: u16,
    pub actions: Vec<(u8, u8, u8, u8, f64, f64, f64)>,
}

#[derive(Debug, Deserialize)]
pub struct RawPlaybookFile {
    pub play_id: String,
    pub min_shot_clock_ds: u16,
    pub allow_mirror: bool,
    pub role_priority: Vec<u8>,
    pub slot_weights: Vec<RawSlotWeights>,
    pub initial_positions: Vec<RawAnchorPos>,
    pub phases: Vec<RawPlayPhase>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
pub enum RawPredicateExpr {
    All { items: Vec<RawPredicateExpr> },
    Any { items: Vec<RawPredicateExpr> },
    Not { inner: Box<RawPredicateExpr> },
    Cmp {
        subject: u8,
        metric_key: RawMetricKey,
        op: u8,
        val: f64,
    },
}

#[derive(Debug, Deserialize)]
pub struct RawRoleDef {
    pub name: String,
    pub expr: RawPredicateExpr,
}

#[derive(Debug, Deserialize)]
pub struct RawPnrRule {
    pub rule_id: String,
    pub predicates: Vec<RawPredicateExpr>,
    pub coverage: PnrCoverageKind,
    pub on_ball_action: u8,
    pub help_rule: u8,
}

#[derive(Debug, Deserialize)]
pub struct RawAdaptationTrigger {
    pub watch_coverage: PnrCoverageKind,
    pub min_possessions: u16,
    pub prior_weight: f64,
    pub prior_ppp: f64,
    pub escalate_ppp_threshold: f64,
    pub deescalate_ppp_threshold: f64,
    pub revert_if_escalated_ppp_exceeds: f64,
    pub revert_min_possessions: u16,
    pub deescalate_min_possessions: u16,
    pub threat_absent_three_point_lt: u8,
    pub cooldown_possessions: u8,
    pub escalate_to_coverage: PnrCoverageKind,
}

#[derive(Debug, Deserialize)]
pub struct RawDefenseFile {
    pub gameplan_id: String,
    pub base_scheme: u8,
    pub switch_max_height_diff_cm: f64,
    pub help_shrink_level: u8,
    #[serde(default)]
    pub role_defs: Vec<RawRoleDef>,
    #[serde(default)]
    pub protect_roles: Vec<String>,
    pub hide_on_weakest_shooter: bool,
    pub pre_switch_enabled: bool,
    pub pre_switch_trigger_radius_m: f64,
    pub scram_switch_enabled: bool,
    pub peel_switch_enabled: bool,
    #[serde(default)]
    pub pnr_rules: Vec<RawPnrRule>,
    #[serde(default)]
    pub adaptation_triggers: Vec<RawAdaptationTrigger>,
}

#[derive(Debug, Deserialize, Default)]
pub struct RawRotationStint {
    pub quarter_mask: u8,
    pub start_game_clock_ds: u16,
    pub end_game_clock_ds: u16,
    pub target_lineup_ranks: Vec<u8>,
}

#[derive(Debug, Deserialize, Default)]
pub struct RawRotationScript {
    #[serde(default)]
    pub stints: Vec<RawRotationStint>,
}

#[derive(Debug, Deserialize)]
pub struct RawTeamTactics {
    pub pace: u8,
    pub transition_push: u8,
    pub default_spacing: u8,
    pub primary_handlers: Vec<u8>,
    pub play_weights: Vec<(String, f64)>,
    pub orb_crash_count: u8,
    pub defensive_gameplan_id: String,
    pub sag_off_3pt_threshold: u8,
    pub foul_up_three_policy: FoulUpThreePolicy,
    #[serde(default)]
    pub matchup_overrides: Vec<(u8, u8)>,
    pub full_court_press_clutch: bool,
    #[serde(default)]
    pub rotation_script: RawRotationScript,
}

#[derive(Debug, Deserialize)]
pub struct RawAttrs {
    pub values: Vec<u8>,
}

#[derive(Debug, Deserialize, Default)]
pub struct RawFunctionalProfileOverride {
    pub primary_creator: Option<f64>,
    pub secondary_handler: Option<f64>,
    pub spacer: Option<f64>,
    pub movement_shooter: Option<f64>,
    pub roll_finisher: Option<f64>,
    pub post_hub: Option<f64>,
    pub point_of_attack: Option<f64>,
    pub rim_protection: Option<f64>,
    pub switchability: Option<f64>,
    pub scores: Option<[f64; 9]>,
    pub guard_tiers: Option<[bool; 5]>,
}

#[derive(Debug, Deserialize)]
pub struct RawPlayer {
    pub player_id: String,
    pub display_name: String,
    pub jersey_number_code: u8,
    #[serde(default)]
    pub badges: Vec<String>,
    pub physical: PhysicalProfile,
    pub tendencies: PlayerTendencies,
    pub attributes: RawAttrs,
    #[serde(default)]
    pub functional_profile_override: Option<RawFunctionalProfileOverride>,
}

#[derive(Debug, Deserialize)]
pub struct RawTeamFile {
    pub team_id: String,
    pub tactics: RawTeamTactics,
    pub players: Vec<RawPlayer>,
}

#[derive(Debug, Clone)]
pub struct ConfigBundle {
    pub nba_ruleset: RuleSet,
    pub fiba_ruleset: RuleSet,
    pub tunables: EngineTunables,
    pub badges: Vec<BadgeDef>,
    pub playbooks: Vec<CompiledPlaybook>,
    pub defenses: Vec<CompiledDefenseGameplan>,
    pub teams: Vec<CompiledTeamSnapshot>,
}

fn read_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, ConfigError> {
    let text = fs::read_to_string(path).map_err(|e| ConfigError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    toml::from_str(&text).map_err(|e| ConfigError::Toml {
        path: path.display().to_string(),
        source: e,
    })
}

fn sorted_toml_files(dir: &Path) -> Result<Vec<PathBuf>, ConfigError> {
    let entries = fs::read_dir(dir).map_err(|e| ConfigError::Io {
        path: dir.display().to_string(),
        source: e,
    })?;
    let mut paths = Vec::new();
    for entry in entries {
        let ent = entry.map_err(|e| ConfigError::Io {
            path: dir.display().to_string(),
            source: e,
        })?;
        let p = ent.path();
        if p.extension().and_then(|s| s.to_str()) == Some("toml") {
            paths.push(p);
        }
    }
    paths.sort();
    Ok(paths)
}

impl ConfigBundle {
    pub fn load_from_dir(root: &Path) -> Result<Self, ConfigError> {
        let nba_ruleset: RuleSet = read_toml(&root.join("rules/nba.toml"))?;
        validate_ruleset(&nba_ruleset)?;
        let fiba_ruleset: RuleSet = read_toml(&root.join("rules/fiba.toml"))?;
        validate_ruleset(&fiba_ruleset)?;

        let tunables: EngineTunables = read_toml(&root.join("tunables/default_v1.toml"))?;
        validate_tunables(&tunables)?;

        let badges_file: BadgesFile = read_toml(&root.join("badges/default.toml"))?;
        let badge_map = validate_badges(&badges_file.badges)?;

        let mut playbooks = Vec::new();
        for p in sorted_toml_files(&root.join("playbooks"))? {
            let raw: RawPlaybookFile = read_toml(&p)?;
            playbooks.push(compile_playbook(raw)?);
        }
        playbooks.sort_by(|a, b| a.play_id.as_str().cmp(b.play_id.as_str()));
        if playbooks.len() > 32 {
            return Err(ConfigError::Validation(format!(
                "playbook count {} exceeds maximum 32",
                playbooks.len()
            )));
        }

        let mut defenses = Vec::new();
        for p in sorted_toml_files(&root.join("defenses"))? {
            let raw: RawDefenseFile = read_toml(&p)?;
            defenses.push(compile_defense(raw)?);
        }
        defenses.sort_by(|a, b| a.gameplan_id.as_str().cmp(b.gameplan_id.as_str()));
        if defenses.len() > 24 {
            return Err(ConfigError::Validation(format!(
                "defense gameplan count {} exceeds maximum 24",
                defenses.len()
            )));
        }
        let mut distinct_rule_ids = std::collections::BTreeSet::new();
        for d in &defenses {
            for r in &d.pnr_rules {
                distinct_rule_ids.insert(r.rule_id.as_str());
            }
        }
        if distinct_rule_ids.len() > 256 {
            return Err(ConfigError::Validation(format!(
                "distinct PnrRuleId count {} exceeds maximum 256",
                distinct_rule_ids.len()
            )));
        }

        // Validate teams against the stricter NBA/FIBA roster limits? Use nba_ruleset (15) or slice for FIBA (12) on MatchInitSpec build
        let mut teams = Vec::new();
        for p in sorted_toml_files(&root.join("teams"))? {
            let raw: RawTeamFile = read_toml(&p)?;
            teams.push(compile_team(raw, &nba_ruleset, &badge_map, &playbooks, &defenses)?);
        }
        teams.sort_by(|a, b| a.team_id.cmp(&b.team_id));

        Ok(Self {
            nba_ruleset,
            fiba_ruleset,
            tunables,
            badges: badges_file.badges,
            playbooks,
            defenses,
            teams,
        })
    }

    pub fn build_match_init_spec(
        &self,
        rule_id: &str,
        home_team_id: &str,
        away_team_id: &str,
        seed: u64,
        state_hash_mode: StateHashMode,
    ) -> Result<MatchInitSpecV1, ConfigError> {
        let ruleset = match rule_id {
            "nba" | "nba_2023_24" => self.nba_ruleset.clone(),
            "fiba" | "fiba_2024" => self.fiba_ruleset.clone(),
            other => {
                return Err(ConfigError::UnknownRef {
                    kind: "ruleset",
                    id: other.to_owned(),
                })
            }
        };

        let mut home_team = self
            .teams
            .iter()
            .find(|t| t.team_id == home_team_id)
            .cloned()
            .ok_or_else(|| ConfigError::UnknownRef {
                kind: "team",
                id: home_team_id.to_owned(),
            })?;
        let mut away_team = self
            .teams
            .iter()
            .find(|t| t.team_id == away_team_id)
            .cloned()
            .ok_or_else(|| ConfigError::UnknownRef {
                kind: "team",
                id: away_team_id.to_owned(),
            })?;

        let max_p = ruleset.roster.max_team_players as usize;
        if home_team.players.len() > max_p {
            home_team.players.truncate(max_p);
        }
        if away_team.players.len() > max_p {
            away_team.players.truncate(max_p);
        }

        let config_hashes = compute_config_hashes(
            &ruleset,
            &self.tunables,
            &self.playbooks,
            &self.defenses,
            &self.badges,
            &home_team,
            &away_team,
        );

        let spec = MatchInitSpecV1 {
            schema_version: 1,
            engine_semver: env!("CARGO_PKG_VERSION").to_owned(),
            engine_git_sha: "workspace-v1".to_owned(),
            rustc_version_sha256: sha256_bytes(b"rustc-1.99.0-stable"),
            engine_wasm_sha256: [0u8; 32],
            cargo_lock_sha256: [0u8; 32],
            seed,
            canonical_team_id_override: None,
            intentional_foul_policy: IntentionalFoulPolicy::Auto,
            config_hashes,
            ruleset,
            tunables: self.tunables.clone(),
            playbooks: self.playbooks.clone(),
            defenses: self.defenses.clone(),
            badges: self.badges.clone(),
            home_team,
            away_team,
            state_hash_mode,
        };

        let size = spec.to_canonical_le_bytes().len();
        if size > MAX_INIT_SPEC_BYTES {
            return Err(ConfigError::InitSpecTooLarge {
                bytes: size,
                limit: MAX_INIT_SPEC_BYTES,
            });
        }

        Ok(spec)
    }
}
