use hoops_core::{AttrIdx, PhysicalProfile, PlayerAttributes38, PlayerFunctionalProfile, PlayerTendencies};

/// 身高对应的自然对位档位（§4.5）。
pub fn natural_tier(height_cm: f64) -> usize {
    if height_cm < 192.0 {
        0
    } else if height_cm <= 198.0 {
        1
    } else if height_cm <= 204.0 {
        2
    } else if height_cm <= 210.0 {
        3
    } else {
        4
    }
}

/// 根据球员属性计算九项功能评分与五项防守覆盖条件（§4.5）。
pub fn derive_functional_profile(
    physical: &PhysicalProfile,
    attrs: &PlayerAttributes38,
    _tendencies: &PlayerTendencies,
) -> PlayerFunctionalProfile {
    use AttrIdx::*;

    let norm = |attr| f64::from(attrs[attr]) / 99.0;
    let s_n = ((physical.standing_reach_cm - 235.0) / 55.0).clamp(0.0, 1.0);
    let w_n = ((physical.weight_kg - 78.0) / 50.0).clamp(0.0, 1.0);

    let primary_creator = (100.0
        * (0.28 * norm(BallHandle)
            + 0.24 * norm(PassVision)
            + 0.20 * norm(PassAccuracy)
            + 0.16 * norm(ShotSelectionIq)
            + 0.12 * norm(Acceleration)))
    .clamp(0.0, 100.0);

    let secondary_handler = (100.0
        * (0.25 * norm(BallHandle)
            + 0.22 * norm(PassAccuracy)
            + 0.20 * norm(OffDribbleShot)
            + 0.18 * norm(Layup)
            + 0.15 * norm(PassVision)))
    .clamp(0.0, 100.0);

    let spacer = (100.0
        * (0.40 * norm(ThreePoint)
            + 0.28 * norm(CatchAndShoot)
            + 0.18 * norm(ShotRange)
            + 0.14 * norm(ShotSelectionIq)))
    .clamp(0.0, 100.0);

    let movement_shooter = (100.0
        * (0.32 * norm(ThreePoint)
            + 0.26 * norm(OffBallMovement)
            + 0.22 * norm(CatchAndShoot)
            + 0.10 * norm(StaminaRating)
            + 0.10 * norm(ContestedShot)))
    .clamp(0.0, 100.0);

    let roll_finisher = (100.0
        * (0.24 * norm(Dunk)
            + 0.20 * norm(Layup)
            + 0.18 * norm(HandsCatch)
            + 0.14 * norm(ScreenSetting)
            + 0.12 * norm(Vertical)
            + 0.12 * s_n))
    .clamp(0.0, 100.0);

    let post_hub = (100.0
        * (0.32 * norm(PostControl)
            + 0.22 * norm(CloseTouch)
            + 0.18 * norm(Strength)
            + 0.14 * norm(PassVision)
            + 0.14 * w_n))
    .clamp(0.0, 100.0);

    let point_of_attack = (100.0
        * (0.32 * norm(PerimeterDefense)
            + 0.26 * norm(LateralAgility)
            + 0.20 * norm(ScreenNavigation)
            + 0.12 * norm(Steal)
            + 0.10 * norm(Hustle)))
    .clamp(0.0, 100.0);

    let rim_protection = (100.0
        * (0.32 * norm(Block)
            + 0.28 * norm(InteriorDefense)
            + 0.16 * s_n
            + 0.14 * norm(Vertical)
            + 0.10 * norm(HelpDefenseIq)))
    .clamp(0.0, 100.0);

    let switchability = (100.0
        * (0.28 * norm(LateralAgility)
            + 0.24 * norm(PerimeterDefense)
            + 0.20 * norm(InteriorDefense)
            + 0.16 * norm(HelpDefenseIq)
            + 0.12 * norm(Strength)))
    .clamp(0.0, 100.0);

    let scores = [
        primary_creator,
        secondary_handler,
        spacer,
        movement_shooter,
        roll_finisher,
        post_hub,
        point_of_attack,
        rim_protection,
        switchability,
    ];

    let mut guard_tiers = [
        physical.height_cm <= 198.0 && attrs[LateralAgility] >= 68 && attrs[PerimeterDefense] >= 62,
        physical.height_cm <= 203.0 && attrs[LateralAgility] >= 62 && attrs[PerimeterDefense] >= 58,
        physical.height_cm >= 195.0
            && physical.height_cm <= 209.0
            && attrs[PerimeterDefense] >= 54
            && attrs[Strength] >= 50,
        physical.height_cm >= 200.0 && (attrs[InteriorDefense] >= 56 || attrs[Strength] >= 60),
        physical.standing_reach_cm >= 262.0
            && (attrs[InteriorDefense] >= 60 || rim_protection >= 58.0)
            && attrs[Strength] >= 62,
    ];
    if !guard_tiers.iter().any(|&v| v) {
        guard_tiers[natural_tier(physical.height_cm)] = true;
    }

    PlayerFunctionalProfile { scores, guard_tiers }
}
