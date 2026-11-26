use std::path::PathBuf;

use clap::Parser;

pub const MAX_ACTIVE_LIVE_GAMES: usize = 100;
pub const MAX_ACTIVE_BATCH_ENGINES: usize = 8;
pub const MAX_WS_CONNECTIONS: usize = 256;
pub const MAX_TRACKED_WAL_SIM_SLOTS: usize = 256;

pub const BATCH_POOL_CAP_BYTES: usize = 192 * 1024 * 1024;
pub const BATCH_POOL_HWM_BYTES: usize = 176 * 1024 * 1024;
pub const LIVE_POOL_CAP_BYTES: usize = 64 * 1024 * 1024;
pub const LIVE_POOL_ADMIT_LIMIT_BYTES: usize = 56 * 1024 * 1024;
pub const CONFIG_CACHE_CAP_BYTES: usize = 42 * 1024 * 1024;
pub const PHASE_B_RETAIN_CAP_BYTES: usize = 12 * 1024 * 1024;
pub const MAX_CONFIG_UPLOAD_BYTES: usize = 512 * 1024;

pub const LIVE_PLAYER_CMD_BUF_BYTES: usize = 44 * 1024;
pub const LIVE_SYSTEM_CMD_RESERVE_BYTES: usize = 20 * 1024;
pub const LIVE_EVENT_LOG_BUF_BYTES: usize = 320 * 1024;
pub const LIVE_EVENT_LOG_MAX_COUNT: usize = 8_192;

pub const WS_QUEUE_MAX_MSGS: usize = 80;
pub const WS_QUEUE_MAX_BYTES: usize = 256 * 1024;
pub const WS_HWM_MSGS: usize = 15;
pub const WS_HWM_BYTES: usize = 128 * 1024;
pub const WS_LWM_MSGS: usize = 4;
pub const WS_LWM_BYTES: usize = 32 * 1024;
pub const WS_STALL_TIMEOUT_MS: u64 = 5_000;
pub const CLOSE_FRAME_WRITE_TIMEOUT_MS: u64 = 500;

pub const WAL_GROUP_COMMIT_MAX_WAIT_MS: u64 = 2;
pub const WAL_GROUP_COMMIT_MAX_RECORDS: usize = 128;
pub const WAL_COMMIT_TIMEOUT_MS: u64 = 2_000;
pub const FINALIZE_FLUSH_TIMEOUT_MS: u64 = 60_000;
pub const PHASE_A_MAX_ROUNDS: usize = 4;
pub const PHASE_A_MAX_WALL_MS: u64 = 15_000;
pub const PHASE_B_MAX_WALL_MS: u64 = 10_000;

#[derive(Debug, Clone, Parser)]
#[command(name = "hoops-server", about = "Hoops concurrent HTTP/WS simulation microservice")]
pub struct ServerConfig {
    /// HTTP + WebSocket listen address (default 0.0.0.0:31110)
    #[arg(long, default_value = "0.0.0.0:31110")]
    pub bind: String,

    /// Directory containing TOML rules, teams, playbooks, defenses, tunables, and badges
    #[arg(long, default_value = "configs")]
    pub configs_dir: PathBuf,

    /// Directory containing static Web UI assets served at /
    #[arg(long, default_value = "viewer")]
    pub viewer_dir: PathBuf,

    /// Shared durable WAL + archive data directory
    #[arg(long, default_value = "data/hoops-server")]
    pub data_dir: PathBuf,

    /// HS256 JWT verification secret
    #[arg(long, default_value = "hoops-dev-hs256-secret")]
    pub jwt_secret: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:31110".to_owned(),
            configs_dir: PathBuf::from("configs"),
            viewer_dir: PathBuf::from("viewer"),
            data_dir: PathBuf::from("data/hoops-server"),
            jwt_secret: "hoops-dev-hs256-secret".to_owned(),
        }
    }
}
