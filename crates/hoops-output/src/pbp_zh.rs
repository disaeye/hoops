use hoops_core::{GameRecordEventPayload, GameRecordEventV1, RuleCode};

use crate::header::Hps1Header;

pub fn format_clock_zh(quarter: u8, ds: u16) -> String {
    let q_str = if quarter <= 4 {
        format!("Q{quarter}")
    } else {
        format!("OT{}", quarter - 4)
    };
    let mins = ds / 600;
    let secs = (ds % 600) / 10;
    let tenths = ds % 10;
    format!("{q_str} {mins:02}:{secs:02}.{tenths}")
}

fn player_label(header: &Hps1Header, roster_idx: u8) -> String {
    if roster_idx as usize >= 30 {
        return format!("#{roster_idx}");
    }
    if let Some(r) = header.lookup_player(roster_idx) {
        if r.jersey_number_code == 100 {
            format!("{}#00", r.display_name)
        } else {
            format!("{}#{}", r.display_name, r.jersey_number_code)
        }
    } else {
        format!("#{roster_idx}")
    }
}

fn side_label(side: u8) -> &'static str {
    match side {
        0 => "主队",
        1 => "客队",
        _ => "系统",
    }
}

fn rule_name_zh(code: u16) -> &'static str {
    match RuleCode::from_u16(code) {
        Some(RuleCode::NONE) => "无犯规/违例",
        Some(RuleCode::VIO_OOB) => "出界违例",
        Some(RuleCode::VIO_SHOT_CLOCK) => "进攻计时器24秒违例",
        Some(RuleCode::VIO_8S) => "8秒不过半场违例",
        Some(RuleCode::VIO_BACKCOURT) => "回场违例",
        Some(RuleCode::VIO_OFF_3S) => "进攻3秒违例",
        Some(RuleCode::VIO_DEF_3S) => "防守3秒违例",
        Some(RuleCode::VIO_5S_INBOUND) => "发球5秒违例",
        Some(RuleCode::VIO_5S_POST) => "背打5秒违例",
        Some(RuleCode::VIO_5S_CLOSELY_GUARDED) => "贴防5秒违例",
        Some(RuleCode::VIO_03S_EXPIRED) => "0.3秒超时出手无效",
        Some(RuleCode::VIO_TRAVEL) => "走步违例",
        Some(RuleCode::VIO_DOUBLE_DRIBBLE) => "两次运球违例",
        Some(RuleCode::VIO_KICKED_BALL) => "脚踢球违例",
        Some(RuleCode::VIO_JUMP_BALL) => "跳球违例",
        Some(RuleCode::VIO_FT_VIOLATION) => "罚球违例",
        Some(RuleCode::VIO_GOALTEND) => "干扰球(Goaltending)",
        Some(RuleCode::VIO_BASKET_INTERFERENCE) => "干扰篮筐",
        Some(RuleCode::HELD_BALL) => "争球",
        Some(RuleCode::WHISTLE_INADVERTENT) => "误哨",
        Some(RuleCode::FOUL_SHOOTING) => "投篮犯规",
        Some(RuleCode::FOUL_REACH_COMMON) => "普通打手/拉人犯规",
        Some(RuleCode::FOUL_AWAY_FROM_PLAY) => "无球犯规",
        Some(RuleCode::FOUL_INBOUND_BEFORE_RELEASE) => "发球前犯规",
        Some(RuleCode::FOUL_BLOCKING) => "阻挡犯规",
        Some(RuleCode::FOUL_CHARGE) => "进攻撞人犯规",
        Some(RuleCode::FOUL_ILLEGAL_SCREEN) => "非法掩护",
        Some(RuleCode::FOUL_LOOSE_BALL) => "抢篮板/自由球犯规",
        Some(RuleCode::FOUL_DOUBLE) => "双方犯规",
        Some(RuleCode::FOUL_TRANSITION_TAKE) => "转换阻断犯规",
        Some(RuleCode::FOUL_CLEAR_PATH) => "路径犯规",
        Some(RuleCode::FOUL_TECH_CONDUCT) => "技术犯规",
        Some(RuleCode::FOUL_TECH_EXCESSIVE_TIMEOUT) => "超额暂停技术犯规",
        Some(RuleCode::FOUL_FLAGRANT_1) => "一级恶意犯规",
        Some(RuleCode::FOUL_FLAGRANT_2) => "二级恶意犯规(驱逐)",
        Some(RuleCode::FOUL_UNSPORTSMANLIKE) => "违体犯规",
        Some(RuleCode::FOUL_DISQUALIFYING) => "取消比赛资格犯规",
        Some(RuleCode::FOUL_TECH_FLOPPING) => "假摔技术犯规",
        None => "未知规则裁决",
    }
}

/// Complete 28-variant Chinese PBP renderer (§10).
pub fn render_event_zh(header: &Hps1Header, ev: &GameRecordEventV1) -> String {
    let clk = format_clock_zh(ev.quarter, ev.game_clock_ds);
    let body = match &ev.payload {
        GameRecordEventPayload::PeriodStarted {
            quarter,
            start_mode,
            inbounding_side,
            ..
        } => {
            let mode = if *start_mode == 0 {
                "中圈跳球开局"
            } else {
                "球权箭头发球"
            };
            format!("第{quarter}节开始（{mode}，发球方：{}）", side_label(*inbounding_side))
        }
        GameRecordEventPayload::JumpBallTapped {
            tip_winner_side,
            tapper_roster_idx,
            ..
        } => format!(
            "{} {} 赢得跳球",
            side_label(*tip_winner_side),
            player_label(header, *tapper_roster_idx)
        ),
        GameRecordEventPayload::PossessionEstablished {
            side,
            handler_roster_idx,
            new_shot_clock_ds,
            ..
        } => format!(
            "{} {} 掌控球权（进攻计时器 {}s）",
            side_label(*side),
            player_label(header, *handler_roster_idx),
            if *new_shot_clock_ds >= 0xFFFE {
                "--".to_owned()
            } else {
                format!("{:.1}", *new_shot_clock_ds as f64 / 10.0)
            }
        ),
        GameRecordEventPayload::PassReleased {
            passer_roster_idx,
            target_roster_idx,
            pass_kind,
            is_extra_pass,
            ..
        } => {
            let kind_str = match *pass_kind {
                0 => "胸前直传",
                1 => "击地/手递手传球",
                2 => "高吊传球",
                3 => "长传快下",
                4 => "界外发球",
                _ => "传球",
            };
            format!(
                "{} {}{}给 {}",
                player_label(header, *passer_roster_idx),
                if *is_extra_pass != 0 { "额外传导" } else { "" },
                kind_str,
                player_label(header, *target_roster_idx)
            )
        }
        GameRecordEventPayload::PassCompleted {
            catcher_roster_idx,
            bobbled_recovered,
            ..
        } => format!(
            "{} 接球成功{}",
            player_label(header, *catcher_roster_idx),
            if *bobbled_recovered != 0 { "（险些失手后救回）" } else { "" }
        ),
        GameRecordEventPayload::PassIntercepted {
            stealer_roster_idx,
            is_deflection_loose,
            ..
        } => format!(
            "{} {}{}",
            player_label(header, *stealer_roster_idx),
            if *is_deflection_loose != 0 { "点拨破坏传球" } else { "抢断完成" },
            ""
        ),
        GameRecordEventPayload::ShotReleased {
            shooter_roster_idx,
            shot_value,
            release_zone,
            c_geom_centi,
            ..
        } => {
            let zone_str = match *release_zone {
                0 => "篮下",
                1 => "近中距离",
                2 => "远中距离",
                3 => "底角",
                4 => "弧顶/翼侧",
                _ => "半场",
            };
            let contest_str = if *c_geom_centi < 15 {
                "空位"
            } else if *c_geom_centi < 45 {
                "轻度干扰"
            } else if *c_geom_centi < 80 {
                "严密干扰"
            } else {
                "强力封堵"
            };
            format!(
                "{} {zone_str}{shot_value}分出手（{contest_str}，压迫度 {c_geom_centi}%）",
                player_label(header, *shooter_roster_idx)
            )
        }
        GameRecordEventPayload::ShotResolved {
            shooter_roster_idx,
            points_awarded,
            is_blocked,
            blocker_roster_idx,
            is_goaltend,
            assist_roster_idx,
            ..
        } => {
            if *is_goaltend != 0 {
                format!(
                    "{} 干扰球算中 {} 分",
                    player_label(header, *shooter_roster_idx),
                    points_awarded
                )
            } else if *is_blocked != 0 {
                format!(
                    "{} 封盖了 {} 的投篮",
                    player_label(header, *blocker_roster_idx),
                    player_label(header, *shooter_roster_idx)
                )
            } else if *points_awarded > 0 {
                let ast = if (*assist_roster_idx as usize) < 30 {
                    format!("（{} 助攻）", player_label(header, *assist_roster_idx))
                } else {
                    String::new()
                };
                format!(
                    "{} 命中 {} 分球！{}",
                    player_label(header, *shooter_roster_idx),
                    points_awarded,
                    ast
                )
            } else {
                format!("{} 投篮不中", player_label(header, *shooter_roster_idx))
            }
        }
        GameRecordEventPayload::FreeThrowResolved {
            shooter_roster_idx,
            made,
            shot_number,
            total_shots,
            ..
        } => format!(
            "{} 罚球（第{}/{}罚）{}",
            player_label(header, *shooter_roster_idx),
            shot_number,
            total_shots,
            if *made != 0 { "命中" } else { "不中" }
        ),
        GameRecordEventPayload::ReboundSecured {
            rebounder_roster_idx,
            rebound_team,
            is_offensive,
            is_deadball_team_reb,
            ..
        } => {
            if *is_deadball_team_reb != 0 {
                format!("{} 获得死球团队篮板", side_label(*rebound_team))
            } else {
                format!(
                    "{} 摘下{}篮板",
                    player_label(header, *rebounder_roster_idx),
                    if *is_offensive != 0 { "前场进攻" } else { "后场防守" }
                )
            }
        }
        GameRecordEventPayload::TurnoverCommitted {
            offender_roster_idx,
            turnover_rule_code,
            stealer_roster_idx,
            is_live_steal,
            ..
        } => {
            if *is_live_steal != 0 {
                format!(
                    "{} 失误，被 {} 抢断",
                    player_label(header, *offender_roster_idx),
                    player_label(header, *stealer_roster_idx)
                )
            } else {
                format!(
                    "{} 失误（{}）",
                    player_label(header, *offender_roster_idx),
                    rule_name_zh(*turnover_rule_code)
                )
            }
        }
        GameRecordEventPayload::ViolationTriggered {
            offender_roster_idx,
            rule_code,
            ..
        } => format!(
            "{} 吹罚{}",
            player_label(header, *offender_roster_idx),
            rule_name_zh(*rule_code)
        ),
        GameRecordEventPayload::RefereeInitialCall {
            offending_side,
            offender_roster_idx,
            initial_rule_code,
            ..
        } => format!(
            "裁判鸣哨：{} {} {}",
            side_label(*offending_side),
            player_label(header, *offender_roster_idx),
            rule_name_zh(*initial_rule_code)
        ),
        GameRecordEventPayload::ChallengeWindowOpenedRecord {
            ruling_id,
            initial_rule_code,
            ..
        } => format!(
            "挑战窗口开启（裁决#{ruling_id}：{}）",
            rule_name_zh(*initial_rule_code)
        ),
        GameRecordEventPayload::CoachChallengeRequested { requester, ruling_id, .. } => format!(
            "{} 教练发起录像挑战（裁决#{ruling_id}）",
            side_label(*requester)
        ),
        GameRecordEventPayload::CoachChallengePendingGrant { requester, .. } => {
            format!("{} 挑战等待裁判批准（FIBA HCC）", side_label(*requester))
        }
        GameRecordEventPayload::CoachChallengeWithdrawn { requester, .. } => {
            format!("{} 撤回本次挑战申请", side_label(*requester))
        }
        GameRecordEventPayload::CoachChallengeReviewed {
            requester,
            overturned,
            final_rule_code,
            ..
        } => format!(
            "录像回放结果（{}发起）：{} -> {}",
            side_label(*requester),
            if *overturned != 0 { "挑战成功改判" } else { "维持原判" },
            rule_name_zh(*final_rule_code)
        ),
        GameRecordEventPayload::RulingFinalized {
            final_rule_code,
            ft_shots_enqueued,
            ..
        } => format!(
            "裁决最终确认：{}（执行 {} 次罚球）",
            rule_name_zh(*final_rule_code),
            ft_shots_enqueued
        ),
        GameRecordEventPayload::UnchallengeableChallengeChargedTimeout {
            requester,
            remaining_timeouts,
            ..
        } => format!(
            "{} 申请不可挑战项目，被扣 1 次暂停（剩余 {}）",
            side_label(*requester),
            remaining_timeouts
        ),
        GameRecordEventPayload::TimeoutGranted {
            reason_tag,
            charged_side,
            home_to_rem,
            away_to_rem,
        } => format!(
            "{}暂停（扣{}，剩余 主{home_to_rem}/客{away_to_rem}）",
            if *reason_tag == 1 { "官方强制" } else { "请求" },
            side_label(*charged_side)
        ),
        GameRecordEventPayload::SubstitutionApplied {
            team_side,
            out_roster_idx,
            in_roster_idx,
            ..
        } => format!(
            "{} 换人：{} 下场，{} 上场",
            side_label(*team_side),
            player_label(header, *out_roster_idx),
            player_label(header, *in_roster_idx)
        ),
        GameRecordEventPayload::DeadTime {
            duration_ds,
            reason_tag,
        } => {
            let label = match *reason_tag {
                0 => "主队请求暂停",
                1 => "客队请求暂停",
                2 => "官方强制暂停",
                3 => "节间休息",
                4 => "中场休息",
                _ => "比赛暂停/死球",
            };
            format!("{label}（时长 {:.1} 秒）", *duration_ds as f64 / 10.0)
        }
        GameRecordEventPayload::AnchoredCommandAbortedRecord {
            ingress_seq,
            abort_reason_code,
            ..
        } => format!("指令#{ingress_seq} 在执行点中止（原因码 0x{abort_reason_code:02X}）"),
        GameRecordEventPayload::ScoringEventAmended {
            shooter_roster_idx,
            old_points,
            new_points,
            ..
        } => format!(
            "回放修正得分：{} 由 {} 分改为 {} 分",
            player_label(header, *shooter_roster_idx),
            old_points,
            new_points
        ),
        GameRecordEventPayload::GameEnded {
            score_home_official,
            score_away_official,
            winner_side,
            ..
        } => format!(
            "比赛结束！官方比分 主队 {score_home_official} : {score_away_official} 客队（胜方：{}）",
            side_label(*winner_side)
        ),
        GameRecordEventPayload::FoulAssessedRecord {
            offending_side,
            offender_roster_idx,
            rule_code,
            resulting_personal_fouls,
            ..
        } => format!(
            "记分台登记犯规：{} {} {}（累计 {} 犯）",
            side_label(*offending_side),
            player_label(header, *offender_roster_idx),
            rule_name_zh(*rule_code),
            resulting_personal_fouls
        ),
        GameRecordEventPayload::ClockAdjusted {
            adjust_reason,
            delta_game_clock_ds_signed,
            new_game_clock_ds,
            ..
        } => {
            let reason_str = match *adjust_reason {
                0 => "Comments II-L 最小走表扣减",
                1 => "回放复核回拨时钟",
                2 => "压哨回补 0.1 秒",
                3 => "技术台官方校表",
                _ => "时钟调整",
            };
            format!(
                "比赛时钟修正（{reason_str}：调整 {:+.1} 秒，剩余 {:.1} 秒）",
                *delta_game_clock_ds_signed as f64 / 10.0,
                *new_game_clock_ds as f64 / 10.0
            )
        }
    };
    format!("[{clk}] {body}")
}
