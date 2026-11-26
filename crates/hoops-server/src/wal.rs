use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hoops_core::{
    sha256_bytes, AnchoredCommand, CanonicalLeBytes, DebugOverlayFrameV1,
    EngineCheckpointSnapshotV1, EngineCursor, GameRecordEventV1, MatchInitSpecV1, PublicFrameV1,
    SimulationEngine, StepStatus, VecSimSink,
};
use hoops_output::LiveBoxScoreState;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::oneshot;

use crate::config::{WAL_GROUP_COMMIT_MAX_RECORDS, WAL_GROUP_COMMIT_MAX_WAIT_MS};

pub const WAL_FRAME_MAGIC: u32 = 0x4857_414C; // "HWAL"

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[repr(u8)]
pub enum StorageFaultDomain {
    Healthy = 0,
    ArchiveStoreOnly = 1,
    WalWriteStalled = 2,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[repr(u8)]
pub enum ServiceTimerKind {
    None = 0,
    WindowClose = 1,
    GrantConfirm = 2,
    DeadTimeDwell = 3,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ServiceTimerState {
    pub kind: u8,
    pub ruling_id: u64,
    pub window_epoch: u32,
    pub request_seq: u32,
    pub deadline_wall_ms: u64,
    pub paused_open_remaining_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveSessionConfigV1 {
    pub schema_version: u16,
    pub sim_id: String,
    pub tick_rate_hz: u8,
    pub pace_multiplier: u8,
    pub home_participant_id: String,
    pub away_participant_id: String,
    pub spectator_policy: u8,
    pub film_room_policy: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SealedChunkMetaV1 {
    pub first_tick: u32,
    pub tick_count: u16,
    pub first_event_seq: u64,
    pub last_event_seq: u64,
    pub writer_lease_epoch: u64,
    pub chunk_bytes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SealedCmdSegMetaV1 {
    pub writer_lease_epoch: u64,
    pub first_ingress_seq: u64,
    pub last_ingress_seq: u64,
    pub uncompressed_bytes: u32,
    pub seg_sha256: [u8; 32],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveServiceCheckpointV1 {
    pub schema_version: u16,
    pub sim_id: String,
    pub cp_gen: u64,
    pub writer_lease_epoch: u64,
    pub wal_applied_seq: u64,
    pub sim_wal_applied_seq: u64,
    pub service_timer: ServiceTimerState,
    pub engine_checkpoint: EngineCheckpointSnapshotV1,
    pub boxscore_state: LiveBoxScoreState,
    pub unsealed_tail_frames: Vec<PublicFrameV1>,
    pub unsealed_debug_frames: Vec<DebugOverlayFrameV1>,
    pub sealed_chunks: Vec<SealedChunkMetaV1>,
    pub sealed_cmd_segs: Vec<SealedCmdSegMetaV1>,
    pub max_sealed_cmd_ingress_seq: u64,
    pub durable_published_frame_tick: u32,
    pub durable_published_rolling_hash: [u8; 32],
    pub durable_published_event_seq: u64,
    pub applied_commands: Vec<AnchoredCommand>,
    pub events_snapshot: Vec<GameRecordEventV1>,
    pub state_hash_checkpoints: Vec<(u32, [u8; 32])>,
    pub missing_frame_ranges: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[repr(u8)]
pub enum WalWriteClass {
    NormalWrite = 0,
    TakeoverCloseoutWrite = 1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[repr(u8)]
pub enum WalRecord {
    AnchoredCommand {
        wal_seq: u64,
        anchored: AnchoredCommand,
        request_fingerprint: [u8; 32],
        post_service_timer: ServiceTimerState,
    } = 0,
    StepCommit {
        wal_seq: u64,
        pre_sim_tick: u32,
        step_count: u8,
        pre_cursor: EngineCursor,
        post_sim_tick: u32,
        post_cursor: EngineCursor,
        post_last_event_seq: u64,
        post_state_hash: [u8; 32],
        post_rolling_hash: [u8; 32],
        post_service_timer: ServiceTimerState,
        durable_completed_frame_tick: u32,
        durable_completed_rolling_hash: [u8; 32],
        durable_published_event_seq: u64,
        expected_resume_sim_tick: u32,
        expected_resume_cursor: EngineCursor,
    } = 1,
    PublishWatermarkOnly {
        watermark_wal_seq: u64,
        durable_completed_frame_tick: u32,
        durable_completed_rolling_hash: [u8; 32],
        durable_published_event_seq: u64,
        expected_resume_sim_tick: u32,
        expected_resume_cursor: EngineCursor,
        post_service_timer: ServiceTimerState,
    } = 2,
}

impl WalRecord {
    pub fn kind(&self) -> u8 {
        match self {
            Self::AnchoredCommand { .. } => 0,
            Self::StepCommit { .. } => 1,
            Self::PublishWatermarkOnly { .. } => 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalSimSlotV1 {
    pub active: bool,
    pub sim_id: String,
    pub lease_epoch: u64,
    pub last_committed_wal_seq: u64,
    pub committed_sim_wal_seq: u64,
    pub durable_checkpoint_gen: u64,
    pub durable_checkpoint_writer_epoch: u64,
    pub durable_checkpoint_wal_seq: u64,
    pub durable_checkpoint_sim_wal_seq: u64,
    pub first_uncheckpointed_wal_seq: u64,
    pub checkpoint_demand_wall_ms: u64,
    pub uncheckpointed_takeover_closeouts: u8,
    pub init_spec_sha256: [u8; 32],
    pub session_config_sha256: [u8; 32],
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WalCommitManifest {
    pub manifest_generation: u64,
    pub authoritative_committed_wal_seq: u64,
    pub oldest_retained_wal_seq: u64,
    pub slots: BTreeMap<String, WalSimSlotV1>,
}

#[derive(Debug, Error)]
pub enum WalError {
    #[error("stale lease epoch (expected {expected}, got {got})")]
    StaleLeaseEpoch { expected: u64, got: u64 },
    #[error("unknown simulation slot {0}")]
    UnknownSim(String),
    #[error("wal io failure: {0}")]
    Io(String),
    #[error("wal corrupted unrecoverable: {0}")]
    Corrupted(String),
    #[error("wal channel closed")]
    Closed,
}

pub fn now_wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Framed binary WAL record serialized on disk:
/// magic(4) + wal_seq(8) + sim_wal_seq(8) + lease_epoch(8) + sim_id_len(2) + sim_id + kind(1) + payload_len(4) + payload(json) + crc32(4)
fn encode_wal_frame_bytes(
    sim_id: &str,
    lease_epoch: u64,
    wal_seq: u64,
    sim_wal_seq: u64,
    record: &WalRecord,
) -> Vec<u8> {
    let payload = serde_json::to_vec(record).expect("serialize WalRecord");
    let sb = sim_id.as_bytes();
    let mut out = Vec::with_capacity(35 + sb.len() + payload.len());
    out.extend_from_slice(&WAL_FRAME_MAGIC.to_le_bytes());
    out.extend_from_slice(&wal_seq.to_le_bytes());
    out.extend_from_slice(&sim_wal_seq.to_le_bytes());
    out.extend_from_slice(&lease_epoch.to_le_bytes());
    out.extend_from_slice(&(sb.len() as u16).to_le_bytes());
    out.extend_from_slice(sb);
    out.push(record.kind());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    let crc = crc32fast::hash(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    out
}

struct PendingCommitItem {
    sim_id: String,
    lease_epoch: u64,
    write_class: WalWriteClass,
    record: WalRecord,
    ack: oneshot::Sender<Result<(u64, u64), WalError>>,
}

struct PendingRegisterCp {
    sim_id: String,
    lease_epoch: u64,
    cp_gen: u64,
    wal_applied_seq: u64,
    sim_wal_applied_seq: u64,
    ack: oneshot::Sender<Result<(), WalError>>,
}

enum CommitterMsg {
    Append(PendingCommitItem),
    RegisterCheckpoint(PendingRegisterCp),
}

struct WalSharedState {
    base_dir: PathBuf,
    manifest: Mutex<WalCommitManifest>,
    fault_domain: Mutex<StorageFaultDomain>,
    wal_batches_total: AtomicU64,
    last_commit_latency_us: AtomicU64,
}

impl WalSharedState {
    fn persist_manifest_locked(&self, m: &mut WalCommitManifest) -> Result<(), WalError> {
        m.manifest_generation += 1;
        let bytes = serde_json::to_vec(m).map_err(|e| WalError::Io(e.to_string()))?;
        let crc = crc32fast::hash(&bytes);
        let mut frame = Vec::with_capacity(8 + bytes.len());
        frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        frame.extend_from_slice(&crc.to_le_bytes());
        frame.extend_from_slice(&bytes);

        // Ping-pong alternation between manifest_a.bin and manifest_b.bin
        let fname = if m.manifest_generation % 2 == 0 {
            "manifest_a.bin"
        } else {
            "manifest_b.bin"
        };
        let target = self.base_dir.join(fname);
        let tmp = self.base_dir.join(format!("{fname}.tmp"));
        let mut f = File::create(&tmp).map_err(|e| WalError::Io(e.to_string()))?;
        f.write_all(&frame).map_err(|e| WalError::Io(e.to_string()))?;
        let _ = f.sync_data();
        fs::rename(&tmp, &target).map_err(|e| WalError::Io(e.to_string()))?;
        Ok(())
    }

    fn load_latest_manifest(base_dir: &Path) -> Result<WalCommitManifest, WalError> {
        let mut best: Option<WalCommitManifest> = None;
        for fname in ["manifest_a.bin", "manifest_b.bin"] {
            let p = base_dir.join(fname);
            if !p.exists() {
                continue;
            }
            let data = fs::read(&p).map_err(|e| WalError::Io(e.to_string()))?;
            if data.len() < 8 {
                continue;
            }
            let len = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
            let expected_crc = u32::from_le_bytes(data[4..8].try_into().unwrap());
            if data.len() != 8 + len {
                continue;
            }
            let actual_crc = crc32fast::hash(&data[8..]);
            if actual_crc != expected_crc {
                continue;
            }
            if let Ok(m) = serde_json::from_slice::<WalCommitManifest>(&data[8..]) {
                if best
                    .as_ref()
                    .is_none_or(|b| m.manifest_generation > b.manifest_generation)
                {
                    best = Some(m);
                }
            }
        }
        Ok(best.unwrap_or_default())
    }
}

#[derive(Clone)]
pub struct SharedDurableWalStore {
    shared: Arc<WalSharedState>,
    tx: mpsc::Sender<CommitterMsg>,
    shutdown: Arc<AtomicBool>,
}

impl Drop for SharedDurableWalStore {
    fn drop(&mut self) {
        if Arc::strong_count(&self.shared) <= 2 {
            self.shutdown.store(true, Ordering::Release);
        }
    }
}

impl SharedDurableWalStore {
    pub fn open(base_dir: impl AsRef<Path>) -> Result<Self, WalError> {
        let dir = base_dir.as_ref().to_path_buf();
        fs::create_dir_all(dir.join("sims")).map_err(|e| WalError::Io(e.to_string()))?;
        let manifest = WalSharedState::load_latest_manifest(&dir)?;
        let shared = Arc::new(WalSharedState {
            base_dir: dir,
            manifest: Mutex::new(manifest),
            fault_domain: Mutex::new(StorageFaultDomain::Healthy),
            wal_batches_total: AtomicU64::new(0),
            last_commit_latency_us: AtomicU64::new(0),
        });

        let (tx, rx) = mpsc::channel::<CommitterMsg>();
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shared = Arc::clone(&shared);
        let worker_shut = Arc::clone(&shutdown);

        std::thread::Builder::new()
            .name("shared-wal-group-committer".to_owned())
            .spawn(move || {
                let wal_path = worker_shared.base_dir.join("wal_log.bin");
                let mut wal_file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .read(true)
                    .open(&wal_path)
                    .expect("open wal_log.bin");

                while !worker_shut.load(Ordering::Acquire) {
                    let first = match rx.recv_timeout(Duration::from_millis(50)) {
                        Ok(m) => m,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };

                    let batch_start = Instant::now();
                    let mut batch = Vec::with_capacity(WAL_GROUP_COMMIT_MAX_RECORDS);
                    batch.push(first);
                    let deadline = batch_start + Duration::from_millis(WAL_GROUP_COMMIT_MAX_WAIT_MS);
                    while batch.len() < WAL_GROUP_COMMIT_MAX_RECORDS {
                        let now = Instant::now();
                        if now >= deadline {
                            break;
                        }
                        match rx.recv_timeout(deadline - now) {
                            Ok(m) => batch.push(m),
                            Err(_) => break,
                        }
                    }

                    let fd = *worker_shared.fault_domain.lock().unwrap();
                    let mut manifest = worker_shared.manifest.lock().unwrap();
                    let mut raw_buf = Vec::with_capacity(64 * 1024);
                    let mut pending_acks: Vec<(
                        oneshot::Sender<Result<(u64, u64), WalError>>,
                        Result<(u64, u64), WalError>,
                    )> = Vec::new();
                    let mut cp_acks: Vec<(oneshot::Sender<Result<(), WalError>>, Result<(), WalError>)> =
                        Vec::new();

                    for msg in batch {
                        match msg {
                            CommitterMsg::Append(item) => {
                                if fd == StorageFaultDomain::WalWriteStalled {
                                    pending_acks.push((
                                        item.ack,
                                        Err(WalError::Io("StorageWalUnavailable".to_owned())),
                                    ));
                                    continue;
                                }
                                let new_wal_seq = manifest.authoritative_committed_wal_seq + 1;
                                let Some(slot) = manifest.slots.get_mut(&item.sim_id) else {
                                    pending_acks.push((item.ack, Err(WalError::UnknownSim(item.sim_id))));
                                    continue;
                                };
                                if slot.lease_epoch != item.lease_epoch {
                                    pending_acks.push((
                                        item.ack,
                                        Err(WalError::StaleLeaseEpoch {
                                            expected: slot.lease_epoch,
                                            got: item.lease_epoch,
                                        }),
                                    ));
                                    continue;
                                }

                                let new_sim_seq = slot.committed_sim_wal_seq + 1;
                                let mut rec = item.record;
                                match &mut rec {
                                    WalRecord::AnchoredCommand { wal_seq, .. } => *wal_seq = new_wal_seq,
                                    WalRecord::StepCommit { wal_seq, .. } => *wal_seq = new_wal_seq,
                                    WalRecord::PublishWatermarkOnly {
                                        watermark_wal_seq, ..
                                    } => *watermark_wal_seq = new_wal_seq,
                                }
                                let frame = encode_wal_frame_bytes(
                                    &item.sim_id,
                                    item.lease_epoch,
                                    new_wal_seq,
                                    new_sim_seq,
                                    &rec,
                                );
                                raw_buf.extend_from_slice(&frame);

                                slot.last_committed_wal_seq = new_wal_seq;
                                slot.committed_sim_wal_seq = new_sim_seq;
                                if slot.first_uncheckpointed_wal_seq == 0 {
                                    slot.first_uncheckpointed_wal_seq = new_wal_seq;
                                }
                                if item.write_class == WalWriteClass::TakeoverCloseoutWrite {
                                    slot.uncheckpointed_takeover_closeouts =
                                        slot.uncheckpointed_takeover_closeouts.saturating_add(1);
                                }
                                manifest.authoritative_committed_wal_seq = new_wal_seq;
                                pending_acks.push((item.ack, Ok((new_wal_seq, new_sim_seq))));
                            }
                            CommitterMsg::RegisterCheckpoint(cp) => {
                                let Some(slot) = manifest.slots.get_mut(&cp.sim_id) else {
                                    cp_acks.push((cp.ack, Err(WalError::UnknownSim(cp.sim_id))));
                                    continue;
                                };
                                if slot.lease_epoch != cp.lease_epoch {
                                    cp_acks.push((
                                        cp.ack,
                                        Err(WalError::StaleLeaseEpoch {
                                            expected: slot.lease_epoch,
                                            got: cp.lease_epoch,
                                        }),
                                    ));
                                    continue;
                                }
                                if cp.cp_gen > slot.durable_checkpoint_gen {
                                    slot.durable_checkpoint_gen = cp.cp_gen;
                                    slot.durable_checkpoint_writer_epoch = cp.lease_epoch;
                                    slot.durable_checkpoint_wal_seq = cp.wal_applied_seq;
                                    slot.durable_checkpoint_sim_wal_seq = cp.sim_wal_applied_seq;
                                    slot.uncheckpointed_takeover_closeouts = 0;
                                    slot.checkpoint_demand_wall_ms = 0;
                                    if slot.committed_sim_wal_seq <= cp.sim_wal_applied_seq {
                                        slot.first_uncheckpointed_wal_seq = 0;
                                    }
                                }
                                cp_acks.push((cp.ack, Ok(())));
                            }
                        }
                    }

                    let mut io_err: Option<String> = None;
                    if !raw_buf.is_empty() {
                        if let Err(e) = wal_file.write_all(&raw_buf) {
                            io_err = Some(e.to_string());
                        } else {
                            let _ = wal_file.sync_data();
                        }
                    }
                    if io_err.is_none() {
                        if let Err(e) = worker_shared.persist_manifest_locked(&mut manifest) {
                            io_err = Some(e.to_string());
                        }
                    }
                    drop(manifest);

                    worker_shared.wal_batches_total.fetch_add(1, Ordering::Relaxed);
                    worker_shared
                        .last_commit_latency_us
                        .store(batch_start.elapsed().as_micros() as u64, Ordering::Relaxed);

                    for (tx_ack, res) in pending_acks {
                        let out = match &io_err {
                            Some(e) => Err(WalError::Io(e.clone())),
                            None => res,
                        };
                        let _ = tx_ack.send(out);
                    }
                    for (tx_ack, res) in cp_acks {
                        let out = match &io_err {
                            Some(e) => Err(WalError::Io(e.clone())),
                            None => res,
                        };
                        let _ = tx_ack.send(out);
                    }
                }
            })
            .map_err(|e| WalError::Io(e.to_string()))?;

        Ok(Self {
            shared,
            tx,
            shutdown,
        })
    }

    pub fn sim_dir(&self, sim_id: &str) -> PathBuf {
        self.shared.base_dir.join("sims").join(sim_id)
    }

    pub fn set_fault_domain(&self, domain: StorageFaultDomain) {
        *self.shared.fault_domain.lock().unwrap() = domain;
    }

    pub fn fault_domain(&self) -> StorageFaultDomain {
        *self.shared.fault_domain.lock().unwrap()
    }

    pub fn stats(&self) -> (u64, u64, u64) {
        let m = self.shared.manifest.lock().unwrap();
        (
            m.authoritative_committed_wal_seq,
            self.shared.wal_batches_total.load(Ordering::Relaxed),
            self.shared.last_commit_latency_us.load(Ordering::Relaxed),
        )
    }

    pub fn register_live_simulation(
        &self,
        sim_id: &str,
        lease_epoch: u64,
        init_spec: &MatchInitSpecV1,
        session_cfg: &LiveSessionConfigV1,
        baseline_cp: &LiveServiceCheckpointV1,
    ) -> Result<([u8; 32], [u8; 32]), WalError> {
        let sdir = self.sim_dir(sim_id);
        fs::create_dir_all(&sdir).map_err(|e| WalError::Io(e.to_string()))?;

        let spec_bytes = init_spec.to_canonical_le_bytes();
        let spec_sha = sha256_bytes(&spec_bytes);
        fs::write(sdir.join("init_spec_v1.bin"), &spec_bytes)
            .map_err(|e| WalError::Io(e.to_string()))?;

        let cfg_bytes = serde_json::to_vec(session_cfg).map_err(|e| WalError::Io(e.to_string()))?;
        let cfg_sha = sha256_bytes(&cfg_bytes);
        fs::write(sdir.join("live_session_config_v1.json"), &cfg_bytes)
            .map_err(|e| WalError::Io(e.to_string()))?;

        let cp_bytes = serde_json::to_vec(baseline_cp).map_err(|e| WalError::Io(e.to_string()))?;
        fs::write(
            sdir.join(format!("epoch_{lease_epoch}_checkpoint_gen_0.bin")),
            &cp_bytes,
        )
        .map_err(|e| WalError::Io(e.to_string()))?;

        let mut m = self.shared.manifest.lock().unwrap();
        m.slots.insert(
            sim_id.to_owned(),
            WalSimSlotV1 {
                active: true,
                sim_id: sim_id.to_owned(),
                lease_epoch,
                last_committed_wal_seq: 0,
                committed_sim_wal_seq: 0,
                durable_checkpoint_gen: 0,
                durable_checkpoint_writer_epoch: lease_epoch,
                durable_checkpoint_wal_seq: 0,
                durable_checkpoint_sim_wal_seq: 0,
                first_uncheckpointed_wal_seq: 0,
                checkpoint_demand_wall_ms: 0,
                uncheckpointed_takeover_closeouts: 0,
                init_spec_sha256: spec_sha,
                session_config_sha256: cfg_sha,
            },
        );
        self.shared.persist_manifest_locked(&mut m)?;
        Ok((spec_sha, cfg_sha))
    }

    pub fn fence_new_lease(&self, sim_id: &str, new_lease_epoch: u64) -> Result<(), WalError> {
        let mut m = self.shared.manifest.lock().unwrap();
        let slot = m
            .slots
            .get_mut(sim_id)
            .ok_or_else(|| WalError::UnknownSim(sim_id.to_owned()))?;
        if new_lease_epoch <= slot.lease_epoch {
            return Err(WalError::StaleLeaseEpoch {
                expected: slot.lease_epoch,
                got: new_lease_epoch,
            });
        }
        slot.lease_epoch = new_lease_epoch;
        self.shared.persist_manifest_locked(&mut m)?;
        Ok(())
    }

    pub fn active_slots_count(&self) -> usize {
        self.shared
            .manifest
            .lock()
            .unwrap()
            .slots
            .values()
            .filter(|s| s.active)
            .count()
    }

    pub fn deregister_terminal_sim(&self, sim_id: &str, lease_epoch: u64) -> Result<(), WalError> {
        let mut m = self.shared.manifest.lock().unwrap();
        if let Some(slot) = m.slots.get_mut(sim_id) {
            if slot.lease_epoch != lease_epoch {
                return Err(WalError::StaleLeaseEpoch {
                    expected: slot.lease_epoch,
                    got: lease_epoch,
                });
            }
            slot.active = false;
            slot.first_uncheckpointed_wal_seq = 0;
            self.shared.persist_manifest_locked(&mut m)?;
        }
        Ok(())
    }

    pub async fn append_record(
        &self,
        sim_id: &str,
        lease_epoch: u64,
        write_class: WalWriteClass,
        record: WalRecord,
    ) -> Result<(u64, u64), WalError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.tx
            .send(CommitterMsg::Append(PendingCommitItem {
                sim_id: sim_id.to_owned(),
                lease_epoch,
                write_class,
                record,
                ack: ack_tx,
            }))
            .map_err(|_| WalError::Closed)?;
        ack_rx.await.map_err(|_| WalError::Closed)?
    }

    pub async fn save_and_register_checkpoint(
        &self,
        sim_id: &str,
        lease_epoch: u64,
        cp: &LiveServiceCheckpointV1,
    ) -> Result<(), WalError> {
        let sdir = self.sim_dir(sim_id);
        let cp_path = sdir.join(format!(
            "epoch_{lease_epoch}_checkpoint_gen_{}.bin",
            cp.cp_gen
        ));
        let tmp_path = sdir.join(format!(
            "epoch_{lease_epoch}_checkpoint_gen_{}.bin.tmp",
            cp.cp_gen
        ));
        let bytes = serde_json::to_vec(cp).map_err(|e| WalError::Io(e.to_string()))?;
        {
            let mut f = File::create(&tmp_path).map_err(|e| WalError::Io(e.to_string()))?;
            f.write_all(&bytes).map_err(|e| WalError::Io(e.to_string()))?;
            let _ = f.sync_data();
        }
        fs::rename(&tmp_path, &cp_path).map_err(|e| WalError::Io(e.to_string()))?;

        let (ack_tx, ack_rx) = oneshot::channel();
        self.tx
            .send(CommitterMsg::RegisterCheckpoint(PendingRegisterCp {
                sim_id: sim_id.to_owned(),
                lease_epoch,
                cp_gen: cp.cp_gen,
                wal_applied_seq: cp.wal_applied_seq,
                sim_wal_applied_seq: cp.sim_wal_applied_seq,
                ack: ack_tx,
            }))
            .map_err(|_| WalError::Closed)?;
        ack_rx.await.map_err(|_| WalError::Closed)?
    }

    /// Reconstruct `MatchInitSpecV1`, `LiveSessionConfigV1`, `SimulationEngine`, and committed tail WAL records for cross-node takeover.
    pub fn recover_simulation_from_wal(
        &self,
        sim_id: &str,
    ) -> Result<
        (
            MatchInitSpecV1,
            LiveSessionConfigV1,
            LiveServiceCheckpointV1,
            SimulationEngine,
            Vec<(u64, u64, WalRecord)>,
        ),
        WalError,
    > {
        let manifest = self.shared.manifest.lock().unwrap().clone();
        let slot = manifest
            .slots
            .get(sim_id)
            .ok_or_else(|| WalError::UnknownSim(sim_id.to_owned()))?;
        let sdir = self.sim_dir(sim_id);

        let spec_bytes =
            fs::read(sdir.join("init_spec_v1.bin")).map_err(|e| WalError::Io(e.to_string()))?;
        if sha256_bytes(&spec_bytes) != slot.init_spec_sha256 {
            return Err(WalError::Corrupted("init_spec sha256 mismatch".to_owned()));
        }
        let init_spec = MatchInitSpecV1::from_canonical_le_bytes(&spec_bytes)
            .map_err(|e| WalError::Corrupted(format!("{e:?}")))?;

        let cfg_bytes = fs::read(sdir.join("live_session_config_v1.json"))
            .map_err(|e| WalError::Io(e.to_string()))?;
        if sha256_bytes(&cfg_bytes) != slot.session_config_sha256 {
            return Err(WalError::Corrupted(
                "session_config sha256 mismatch".to_owned(),
            ));
        }
        let session_cfg: LiveSessionConfigV1 =
            serde_json::from_slice(&cfg_bytes).map_err(|e| WalError::Corrupted(e.to_string()))?;

        let cp_path = sdir.join(format!(
            "epoch_{}_checkpoint_gen_{}.bin",
            slot.durable_checkpoint_writer_epoch, slot.durable_checkpoint_gen
        ));
        let cp_bytes = fs::read(&cp_path).map_err(|e| WalError::Io(e.to_string()))?;
        let cp: LiveServiceCheckpointV1 =
            serde_json::from_slice(&cp_bytes).map_err(|e| WalError::Corrupted(e.to_string()))?;

        let mut null_sink = VecSimSink::default();
        let mut engine = SimulationEngine::from_checkpoint_snapshot(
            &init_spec,
            cp.engine_checkpoint.clone(),
            &mut null_sink,
        )
        .map_err(|e| WalError::Corrupted(format!("checkpoint engine init: {e:?}")))?;

        // Scan wal_log.bin bounded by authoritative_committed_wal_seq and > cp.wal_applied_seq
        let wal_path = self.shared.base_dir.join("wal_log.bin");
        let tail_records = if wal_path.exists() {
            read_committed_sim_records(
                &wal_path,
                sim_id,
                manifest.authoritative_committed_wal_seq,
                cp.wal_applied_seq,
            )?
        } else {
            Vec::new()
        };

        let mut sink = VecSimSink::default();
        for (_wseq, _sseq, rec) in &tail_records {
            match rec {
                WalRecord::AnchoredCommand { anchored, .. } => {
                    engine
                        .replay_anchored_command(*anchored, &mut sink)
                        .map_err(|e| WalError::Corrupted(format!("replay anchored: {e:?}")))?;
                }
                WalRecord::StepCommit { step_count, .. } => {
                    for _ in 0..*step_count {
                        let st = engine.step(&mut sink);
                        if matches!(st, StepStatus::Finished { .. } | StepStatus::Aborted { .. }) {
                            break;
                        }
                    }
                }
                WalRecord::PublishWatermarkOnly { .. } => {}
            }
        }
        Ok((init_spec, session_cfg, cp, engine, tail_records))
    }
}

fn read_committed_sim_records(
    wal_path: &Path,
    sim_id: &str,
    auth_seq: u64,
    after_wal_seq: u64,
) -> Result<Vec<(u64, u64, WalRecord)>, WalError> {
    let mut f = File::open(wal_path).map_err(|e| WalError::Io(e.to_string()))?;
    let mut buf = Vec::new();
    f.seek(SeekFrom::Start(0))
        .map_err(|e| WalError::Io(e.to_string()))?;
    f.read_to_end(&mut buf)
        .map_err(|e| WalError::Io(e.to_string()))?;

    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 35 <= buf.len() {
        let magic = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap());
        if magic != WAL_FRAME_MAGIC {
            break;
        }
        let wal_seq = u64::from_le_bytes(buf[pos + 4..pos + 12].try_into().unwrap());
        let sim_wal_seq = u64::from_le_bytes(buf[pos + 12..pos + 20].try_into().unwrap());
        let _lease_epoch = u64::from_le_bytes(buf[pos + 20..pos + 28].try_into().unwrap());
        let id_len = u16::from_le_bytes(buf[pos + 28..pos + 30].try_into().unwrap()) as usize;
        if pos + 30 + id_len + 5 > buf.len() {
            // Uncommitted tail short write
            if wal_seq > auth_seq {
                break;
            }
            return Err(WalError::Corrupted(format!(
                "truncated committed record wal_seq={wal_seq}"
            )));
        }
        let rec_sim_id = std::str::from_utf8(&buf[pos + 30..pos + 30 + id_len])
            .map_err(|_| WalError::Corrupted("invalid utf8 sim_id in wal".to_owned()))?;
        let _kind = buf[pos + 30 + id_len];
        let p_len_off = pos + 30 + id_len + 1;
        let payload_len =
            u32::from_le_bytes(buf[p_len_off..p_len_off + 4].try_into().unwrap()) as usize;
        let p_start = p_len_off + 4;
        let frame_end = p_start + payload_len + 4;
        if frame_end > buf.len() {
            if wal_seq > auth_seq {
                break;
            }
            return Err(WalError::Corrupted(format!(
                "truncated committed payload wal_seq={wal_seq}"
            )));
        }
        let expected_crc =
            u32::from_le_bytes(buf[p_start + payload_len..frame_end].try_into().unwrap());
        let actual_crc = crc32fast::hash(&buf[pos..p_start + payload_len]);
        if actual_crc != expected_crc {
            if wal_seq > auth_seq {
                break;
            }
            return Err(WalError::Corrupted(format!(
                "crc mismatch on committed wal_seq={wal_seq}"
            )));
        }
        pos = frame_end;
        if wal_seq > auth_seq {
            break;
        }
        if wal_seq > after_wal_seq && rec_sim_id == sim_id {
            let rec: WalRecord = serde_json::from_slice(&buf[p_start..p_start + payload_len])
                .map_err(|e| WalError::Corrupted(e.to_string()))?;
            out.push((wal_seq, sim_wal_seq, rec));
        }
    }
    Ok(out)
}
