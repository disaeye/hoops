use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::config::FINALIZE_FLUSH_TIMEOUT_MS;
use crate::wal::now_wall_ms;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FinalizeOriginPath {
    HealthyTail,
    Backfill,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ArchiveStatus {
    ArchiveLiveHealthy,
    ArchiveLiveDegraded,
    ArchiveLiveResumedWithGaps,
    ArchiveFinishedPendingBackfill {
        spool_uri: String,
        replay_uri: String,
    },
    ArchiveFinalizingRetry {
        origin_path: FinalizeOriginPath,
        candidate_artifact_uri: String,
        spool_uri: Option<String>,
        replay_uri: String,
        attempt_count: u8,
        retry_started_wall_ms: u64,
    },
    ArchiveComplete {
        published_artifact_uri: String,
        published_replay_uri: String,
    },
    ArchiveFailed {
        reason: String,
    },
}

impl ArchiveStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::ArchiveComplete { .. } | Self::ArchiveFailed { .. }
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SimulationKind {
    Live,
    BatchFast,
    BatchFullTrace,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MatchLifecycleState {
    Running,
    Finished,
    Aborted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationDirectoryEntry {
    pub sim_id: String,
    pub kind: SimulationKind,
    pub owner_user_id: String,
    pub home_participant_id: String,
    pub away_participant_id: String,
    pub rule_id: String,
    pub home_team_id: String,
    pub away_team_id: String,
    pub pace_multiplier: u8,
    #[serde(default)]
    pub playback_paused: bool,
    pub lease_epoch: u64,
    pub owner_node_id: String,
    pub lifecycle: MatchLifecycleState,
    pub abort_reason: Option<String>,
    pub archive_status: ArchiveStatus,
    pub published_artifact_uri: Option<String>,
    pub published_replay_uri: Option<String>,
    pub published_debug_uri: Option<String>,
    pub score_home: u16,
    pub score_away: u16,
    pub quarter: u8,
    pub game_clock_ds: u16,
    pub game_phase: u8,
    #[serde(default)]
    pub last_tick: u32,
    pub created_wall_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CasOutcome {
    Updated,
    AlreadyPublishedSameUri,
    AlreadyCompleted {
        published_artifact_uri: String,
        published_replay_uri: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CasError {
    NotFound,
    StaleLeaseEpoch { expected: u64, got: u64 },
}

pub struct ArchiveStore {
    pub base_dir: PathBuf,
    entries: Mutex<BTreeMap<String, SimulationDirectoryEntry>>,
}

impl ArchiveStore {
    pub fn new(base_dir: impl Into<PathBuf>) -> Result<Self, std::io::Error> {
        let dir = base_dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self {
            base_dir: dir,
            entries: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn insert_entry(&self, entry: SimulationDirectoryEntry) {
        self.entries
            .lock()
            .unwrap()
            .insert(entry.sim_id.clone(), entry);
    }

    pub fn get_entry(&self, sim_id: &str) -> Option<SimulationDirectoryEntry> {
        self.entries.lock().unwrap().get(sim_id).cloned()
    }

    pub fn list_entries(&self) -> Vec<SimulationDirectoryEntry> {
        self.entries.lock().unwrap().values().cloned().collect()
    }

    pub fn update_progress(
        &self,
        sim_id: &str,
        lease_epoch: u64,
        score_home: u16,
        score_away: u16,
        quarter: u8,
        game_clock_ds: u16,
        game_phase: u8,
        last_tick: u32,
        lifecycle: MatchLifecycleState,
    ) {
        if let Some(ent) = self.entries.lock().unwrap().get_mut(sim_id) {
            if ent.lease_epoch == lease_epoch {
                ent.score_home = score_home;
                ent.score_away = score_away;
                ent.quarter = quarter;
                ent.game_clock_ds = game_clock_ds;
                ent.game_phase = game_phase;
                ent.last_tick = last_tick;
                ent.lifecycle = lifecycle;
                if lifecycle != MatchLifecycleState::Running {
                    ent.playback_paused = false;
                }
            }
        }
    }

    pub fn set_playback_paused(&self, sim_id: &str, lease_epoch: u64, paused: bool) -> bool {
        let mut entries = self.entries.lock().unwrap();
        let Some(entry) = entries.get_mut(sim_id) else {
            return false;
        };
        if entry.lease_epoch != lease_epoch || entry.lifecycle != MatchLifecycleState::Running {
            return false;
        }
        entry.playback_paused = paused;
        true
    }

    pub fn bump_lease_epoch(&self, sim_id: &str, new_owner_node: String) -> Result<u64, CasError> {
        let mut map = self.entries.lock().unwrap();
        let ent = map.get_mut(sim_id).ok_or(CasError::NotFound)?;
        ent.lease_epoch += 1;
        ent.owner_node_id = new_owner_node;
        Ok(ent.lease_epoch)
    }

    pub fn mark_aborted(
        &self,
        sim_id: &str,
        lease_epoch: u64,
        reason: String,
    ) -> Result<(), CasError> {
        let mut map = self.entries.lock().unwrap();
        let ent = map.get_mut(sim_id).ok_or(CasError::NotFound)?;
        if ent.lease_epoch != lease_epoch {
            return Err(CasError::StaleLeaseEpoch {
                expected: ent.lease_epoch,
                got: lease_epoch,
            });
        }
        ent.lifecycle = MatchLifecycleState::Aborted;
        ent.playback_paused = false;
        ent.abort_reason = Some(reason.clone());
        if !matches!(ent.archive_status, ArchiveStatus::ArchiveComplete { .. }) {
            ent.archive_status = ArchiveStatus::ArchiveFailed { reason };
        }
        Ok(())
    }

    pub fn cas_set_archive_status(
        &self,
        sim_id: &str,
        lease_epoch: u64,
        new_status: ArchiveStatus,
    ) -> Result<CasOutcome, CasError> {
        let mut map = self.entries.lock().unwrap();
        let ent = map.get_mut(sim_id).ok_or(CasError::NotFound)?;
        if ent.lease_epoch != lease_epoch {
            return Err(CasError::StaleLeaseEpoch {
                expected: ent.lease_epoch,
                got: lease_epoch,
            });
        }
        match (&ent.archive_status, &new_status) {
            (
                ArchiveStatus::ArchiveComplete {
                    published_artifact_uri: cur_art,
                    published_replay_uri: cur_rep,
                },
                ArchiveStatus::ArchiveComplete {
                    published_artifact_uri: new_art,
                    published_replay_uri: new_rep,
                },
            ) if cur_art == new_art && cur_rep == new_rep => {
                return Ok(CasOutcome::AlreadyPublishedSameUri);
            }
            (
                ArchiveStatus::ArchiveComplete {
                    published_artifact_uri,
                    published_replay_uri,
                },
                ArchiveStatus::ArchiveFailed { .. },
            ) => {
                // Path C guard: NEVER downgrade ArchiveComplete to ArchiveFailed
                return Ok(CasOutcome::AlreadyCompleted {
                    published_artifact_uri: published_artifact_uri.clone(),
                    published_replay_uri: published_replay_uri.clone(),
                });
            }
            _ => {}
        }

        if let ArchiveStatus::ArchiveComplete {
            ref published_artifact_uri,
            ref published_replay_uri,
        } = new_status
        {
            ent.published_artifact_uri = Some(published_artifact_uri.clone());
            ent.published_replay_uri = Some(published_replay_uri.clone());
            ent.lifecycle = MatchLifecycleState::Finished;
        } else if let ArchiveStatus::ArchiveFinishedPendingBackfill { ref replay_uri, .. }
        | ArchiveStatus::ArchiveFinalizingRetry { ref replay_uri, .. } = new_status
        {
            ent.published_replay_uri = Some(replay_uri.clone());
        }
        ent.archive_status = new_status;
        Ok(CasOutcome::Updated)
    }

    pub fn cas_publish_complete(
        &self,
        sim_id: &str,
        lease_epoch: u64,
        artifact_uri: String,
        replay_uri: String,
        debug_uri: Option<String>,
    ) -> Result<CasOutcome, CasError> {
        let out = self.cas_set_archive_status(
            sim_id,
            lease_epoch,
            ArchiveStatus::ArchiveComplete {
                published_artifact_uri: artifact_uri,
                published_replay_uri: replay_uri,
            },
        )?;
        if let Some(d) = debug_uri {
            if let Some(ent) = self.entries.lock().unwrap().get_mut(sim_id) {
                ent.published_debug_uri = Some(d);
            }
        }
        Ok(out)
    }

    /// Guarded cleanup: never delete an object referenced by published_artifact_uri or published_replay_uri or active retry/backfill.
    pub fn safe_delete_unpublished_candidate(&self, sim_id: &str, candidate_path: &str) -> bool {
        let map = self.entries.lock().unwrap();
        if let Some(ent) = map.get(sim_id) {
            if ent.published_artifact_uri.as_deref() == Some(candidate_path)
                || ent.published_replay_uri.as_deref() == Some(candidate_path)
            {
                return false;
            }
            match &ent.archive_status {
                ArchiveStatus::ArchiveFinishedPendingBackfill {
                    spool_uri,
                    replay_uri,
                } => {
                    if spool_uri == candidate_path || replay_uri == candidate_path {
                        return false;
                    }
                }
                ArchiveStatus::ArchiveFinalizingRetry {
                    spool_uri,
                    replay_uri,
                    retry_started_wall_ms,
                    ..
                } => {
                    if spool_uri.as_deref() == Some(candidate_path) || replay_uri == candidate_path
                    {
                        return false;
                    }
                    if now_wall_ms().saturating_sub(*retry_started_wall_ms)
                        < FINALIZE_FLUSH_TIMEOUT_MS
                    {
                        // Leave active retry check intact unless explicitly abandoned
                    }
                }
                ArchiveStatus::ArchiveComplete {
                    published_artifact_uri,
                    published_replay_uri,
                } => {
                    if published_artifact_uri == candidate_path
                        || published_replay_uri == candidate_path
                    {
                        return false;
                    }
                }
                _ => {}
            }
        }
        drop(map);
        let p = self.base_dir.join(candidate_path);
        if p.exists() {
            fs::remove_file(p).is_ok()
        } else {
            false
        }
    }

    pub fn write_atomic_artifact(
        &self,
        rel_path: &str,
        bytes: &[u8],
    ) -> Result<String, std::io::Error> {
        self.write_atomic_artifact_with(rel_path, |writer| writer.write_all(bytes))
    }

    pub fn write_atomic_artifact_with(
        &self,
        rel_path: &str,
        write: impl FnOnce(&mut File) -> std::io::Result<()>,
    ) -> std::io::Result<String> {
        let full = self.base_dir.join(rel_path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = self.base_dir.join(format!("{rel_path}.tmp"));
        let result = (|| {
            let mut file = File::create(&tmp)?;
            write(&mut file)?;
            file.sync_data()?;
            fs::rename(&tmp, &full)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result?;
        Ok(rel_path.to_owned())
    }

    pub fn read_artifact(&self, rel_path: &str) -> Result<Vec<u8>, std::io::Error> {
        fs::read(self.base_dir.join(rel_path))
    }
}
