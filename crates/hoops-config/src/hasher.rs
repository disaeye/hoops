use hoops_core::{
    sha256_bytes, BadgeDef, CanonicalLeBytes, CompiledDefenseGameplan, CompiledPlaybook,
    CompiledTeamSnapshot, ConfigHashes, EngineTunables, RuleSet,
};

pub fn compute_config_hashes(
    ruleset: &RuleSet,
    tunables: &EngineTunables,
    playbooks: &[CompiledPlaybook],
    defenses: &[CompiledDefenseGameplan],
    badges: &[BadgeDef],
    home_team: &CompiledTeamSnapshot,
    away_team: &CompiledTeamSnapshot,
) -> ConfigHashes {
    let mut roster_bytes = home_team.to_canonical_le_bytes();
    roster_bytes.extend_from_slice(&away_team.to_canonical_le_bytes());

    ConfigHashes {
        ruleset_sha256: sha256_bytes(&ruleset.to_canonical_le_bytes()),
        tunables_sha256: sha256_bytes(&tunables.to_canonical_le_bytes()),
        playbooks_sha256: sha256_bytes(&playbooks.to_vec().to_canonical_le_bytes()),
        defenses_sha256: sha256_bytes(&defenses.to_vec().to_canonical_le_bytes()),
        badges_sha256: sha256_bytes(&badges.to_vec().to_canonical_le_bytes()),
        rosters_sha256: sha256_bytes(&roster_bytes),
    }
}
