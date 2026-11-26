# Hoops 篮球比赛模拟引擎 — 工程落地实施计划（M0–M6 + Web 监听端口 `31110`）

> 本文档为 `docs/ARCHITECTURE.md`（v30）的工程实施落盘清单，涵盖 6 Crate 多工作区拓扑、冻结协议边界、`configs/` 数据集、`viewer/` 零构建前端、WASM 交叉验证与 `hoops-server` 默认 `0.0.0.0:31110` 部署说明。

---

## 1. 工作区拓扑与单向依赖约束

```text
hoops/
├── Cargo.toml
├── rust-toolchain.toml          # channel = "stable", targets = ["wasm32-unknown-unknown"]
├── .clippy.toml
├── docs/
│   ├── ARCHITECTURE.md          # Single Source of Truth (v30)
│   └── IMPLEMENTATION_PLAN.md   # 本工程实施落地计划
├── configs/
│   ├── rules/{nba,fiba}.toml
│   ├── tunables/default_v1.toml
│   ├── badges/default.toml
│   ├── playbooks/*.toml         # 8 套相对锚点进攻剧本
│   ├── defenses/*.toml          # 4 套强类型防守比赛计划
│   ├── teams/*.toml             # 4 支典型 NBA 样板队
│   └── benchmarks/              # holdout manifest / PBP IDs / micro tracking schema
├── crates/
│   ├── hoops-core/              # 零 IO 纯确定性 10 Hz 内核 + wire_schema_v1 + WASM C-ABI
│   ├── hoops-config/            # TOML/JSON 加载、DoS 门禁、DSL 编译、ConfigHashes
│   ├── hoops-output/            # HPS1/RawStream/SoADeltaZstd600/.hreplay/BoxScore/中文 PBP
│   ├── hoops-audit/             # Linter / CRN Bootstrap A/B / Layer 3 Metrics 1-6 / Bisect / Calibrate
│   ├── hoops-server/            # Axum HTTP/WS (:31110)、SharedDurableWalStore、Rayon Batch + Tokio Live
│   └── hoops-cli/               # run / trace-hashes / bisect / lint / audit / calibrate / bench
├── scripts/
│   ├── verify_wasm.mjs          # Node WebAssembly 验证 14 导出、<=64 pages、Native/WASM 一致性
│   └── verify_viewer_codec.mjs  # Node ES Module 验证 viewer/codec.js 与 Rust 黄金二进制向量
└── viewer/                      # 零构建 HTML5 Canvas 2D 录像室与实时对战控制台
    ├── index.html
    ├── styles.css
    ├── codec.js
    ├── renderer.js
    └── app.js
```

单向无环依赖：
- `hoops-core`：仅依赖 `serde`, `libm`, `sha2 (default-features=false)`, `thiserror`。禁止 `HashMap`/`HashSet`、`std` 超越函数、文件/网络/时钟 IO。
- `hoops-config` -> `hoops-core`
- `hoops-output` -> `hoops-core`, `hoops-config`
- `hoops-audit` -> `hoops-core`, `hoops-config`, `hoops-output`
- `hoops-server` -> `hoops-core`, `hoops-config`, `hoops-output`
- `hoops-cli` -> `hoops-core`, `hoops-config`, `hoops-output`, `hoops-audit`

---

## 2. 里程碑实施矩阵（M0–M6）

| 里程碑 | 核心模块与功能范围 | 关键工程不变量与配额 |
| :--- | :--- | :--- |
| **M0** | `CanonicalLeBytes`（8 条规则、`_pad == 0`）、`wire_schema_v1`（`EngineTunables` 113 字段 / 153 标量、`38` 项 `RuleCode`、`28` 项 `CommandRejectReason`、`11` 项 `CommandAbortReason`、`GameState` 7 大类、28 类 `GameRecordEventV1` `0x01..=0x1C`、`HTR1` `80 B` 头）、`Philox4x32_10`、四域 SHA-256 链、`WasmEngineHandle` 14 导出 + `0xFFFF_FFFE` | `ContactAndFoulRoll <= 64` Debug/Release 统一 `Aborted`；`MatchInitSpecV1 <= 256.0 KiB`；`ReplayManifest <= 2.0 MiB`（极值 `1,423,761 B`）；WASM `<= 64 pages (4.0 MiB)` 且 `step()` 零增长 |
| **M1** | 2D Swept CCD（128 轮 SOR + 4×128 重线性 Dykstra + `4×3` 格点兜底 + 32 轮同时簇 TOI + 64 次止于表面锁）、3D 抛物线 + 5 层干扰球裁决、`10 µs` 亚分秒三时钟、1-Tick `DeadTime`、`HPS1` + `RawStream`、19 项 FSM 临界测试 + 9 项物理测试 | `PlayableBounds >= 32.0×19.0 m`；审计子段穿透 `<= 0.5 mm`；`sim_tick` 全场 `+1` 连续 `0..=T_final`；`frame_v1_golden` 跨端验证 |
| **M2** | `PlayerFunctionalProfile` 线性派生 + `guard_tiers`、相对锚点 Playbook DSL + $K!$ 双射分配 + 自环防呆、强类型 `DefensiveGameplan` + 经验贝叶斯自适应、12 字段 `FootworkRecord`、有界 2-Ply EPV（`d ∈ {0,1,2}`）、闭式 5 参 Endgame WP + `10 µs` 离手门控、2D `d_pen` 协防三角 | `EPV_drive(k, d)` 不双扣 `d`；M2 3 项收官情景（领先 1 分控球禁提前投、差 3 分舍 2 投 3、`10 µs` 前/同/后三态边界）；`lambda_help` 严格单调 |
| **M3** | NBA 2023–24 vs FIBA 2024 全 38 项 `RuleCode`、Two-Stage Commit Point、NBA Rule 13/14 挑战真值表与 §VI 保留得分、FIBA Appendix F.3/F.4 HCC（获准即扣、双槽调度、`BoundedReviewableEventRing<256>` Tier-1/Tier-2）、Art. 17.2.4 选边钳制、Art. 18/19 `post_made_fg` vs `post_made_last_ft` 分轨与 Art. 19.2.4 `FibaSubReentryLock`（`Δu > 0` 解锁）、NBA 5 人犯满留场与伤员替罚、FIBA `<2` 人判负、`ForfeitMatch` 统一 Stage 8 | 10 组端到端内存 `ReplayManifest` 等价重演与 Native/WASM 逐拍哈希一致；`MAX_SYSTEM_COMMANDS_PER_GAME = 8,192` 耗尽直通完赛；`COMMIT_EVENT_RESERVE = 10` 不阻塞关窗与认输 |
| **M4** | 活球基准双层体力（`burst_energy` + `cumulative_stamina`）、1-Tick `DeadTime` 离散恢复、六级全序轮换（`P1 ≻ P0 ≻ P2 ≻ P3 ≻ P4 ≻ P5`、`P0` 1,200 活球拍锁）、`intentional_foul_mode`、`foul_up_three_policy` | 48 min 比赛上场时间梯队分布验证、`P0` 锁期零 AI 逆换违例 |
| **M5** | `SoADeltaZstd600`（600 帧 / 2,048 事件 / 512 KiB 切块、`<= 528 KiB` 压缩、半块降级、`SeekIndexFooterV1`）、`.hreplay`（`b"HRPL"`）、`LiveBoxScoreState`（跨流同拍帧优先屏障、`PausedInPostWhistle` 帧滞后挂起、`MIN` 与 `0x1C` 正交闭合、`ft_flags.bit0`）、中文 PBP、4 样板队、`hoops-audit`（Linter / CRN Bootstrap A/B / Layer 3 Metrics 1–6）、`hoops-cli` | 删净原始 TOML/JSON 后 `.hreplay` 自包含重演 Native/WASM 一致；`archive_soa_zstd_v1_golden` Rust/JS 双端逐字节一致 |
| **M6** | `hoops-server` 绑定 **`0.0.0.0:31110`**（REST + WS + 静态 `viewer/`）：Rayon `MAX_ACTIVE_BATCH_ENGINES = 8` + Tokio `select! { biased; }` Live Actor（`pace_multiplier ∈ {1,2,4,5}`、`Flush-on-Window`/`Flush-on-DeadTime` + `DeadTimeDwell`）、`SharedDurableWalStore` + `SharedWalGroupCommitter`（32×2 MiB 循环段含 2 段接管保留、双槽 64 KiB `WalCommitManifest`、检查点两步串行协议与 `scan_pin`/`checkpoint_pins`、跨节点无损接管 4 步恢复）、不可变 CAS 三路径归档、`ByteQuotaPool`（478 MiB 预算 / `VmHWM < 512 MiB`）、慢消费双水位 + `close_control_slot <= 550 ms` + 两阶段重连（`AwaitSealingFlush` + `boxscore_snapshot`）、`4003`/`4008`–`4013` 关闭码 | 全部 REST/WS 端点（含 `/forfeit`、PvP 禁 `DELETE`、`FastBatch` `409`、`Live` 未完赛 `/replay` `403`）；`viewer/` 实时对局与 2D Film-Room 回放于 `http://127.0.0.1:31110` 可直接交互使用 |

---

## 3. Web 服务端口与运行说明

- 默认监听地址：**`0.0.0.0:31110`**（可通过 `--port 31110` / `--bind 0.0.0.0:31110` / 环境变量 `HOOPS_PORT=31110` / `HOOPS_BIND=0.0.0.0:31110` 配置）。
- 访问地址：`http://127.0.0.1:31110/`（2D 录像室与实时对战控制台）；WebSocket 端点：`ws://127.0.0.1:31110/api/v1/ws/simulations/{sim_id}`。
