//! Shared workspace persistence, authority checks, and routing snapshots.
//!
//! Configuration revision (`WorkspaceConfig::revision`) is independent of live
//! per-device topology `generation`/`epoch`. Missing `workspace.toml` means
//! legacy Position-only operation.

use input_event::workspace::{
    DevicePlacement, LocalTopology, PeerFingerprint, RoutingSnapshot, WorkspaceConfig,
    WorkspaceError, derive_routes,
};
use mousehop_proto::ProtoEvent;
use mousehop_proto::workspace_wire::{
    decode_workspace_payload, encode_workspace_payload, try_reassemble_workspace, workspace_chunks,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

const WORKSPACE_FILE_NAME: &str = "workspace.toml";

#[derive(Debug, Error)]
pub enum WorkspaceStateError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    TomlSer(#[from] toml::ser::Error),
    #[error(transparent)]
    TomlDe(#[from] toml::de::Error),
    #[error(transparent)]
    Geometry(#[from] WorkspaceError),
    #[error("unauthorized workspace publisher: {0}")]
    Unauthorized(String),
    #[error("stale workspace revision {got} (have {have})")]
    StaleRevision { got: u64, have: u64 },
    #[error("workspace revision conflict: incoming {incoming} vs local {local}")]
    RevisionConflict { incoming: u64, local: u64 },
    #[error("invalid workspace payload")]
    InvalidPayload,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct TomlPlacement {
    fingerprint: String,
    origin_x: i32,
    origin_y: i32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct TomlWorkspace {
    workspace_id: String,
    revision: u64,
    authority_fingerprint: String,
    #[serde(default)]
    placements: Vec<TomlPlacement>,
}

impl From<&WorkspaceConfig> for TomlWorkspace {
    fn from(config: &WorkspaceConfig) -> Self {
        Self {
            workspace_id: config.workspace_id.clone(),
            revision: config.revision,
            authority_fingerprint: config.authority_fingerprint.clone(),
            placements: config
                .placements
                .iter()
                .map(|p| TomlPlacement {
                    fingerprint: p.peer_fingerprint.clone(),
                    origin_x: p.origin_in_workspace.0,
                    origin_y: p.origin_in_workspace.1,
                })
                .collect(),
        }
    }
}

impl TryFrom<TomlWorkspace> for WorkspaceConfig {
    type Error = WorkspaceError;

    fn try_from(toml: TomlWorkspace) -> Result<Self, Self::Error> {
        let config = WorkspaceConfig {
            workspace_id: toml.workspace_id,
            revision: toml.revision,
            authority_fingerprint: toml.authority_fingerprint,
            placements: toml
                .placements
                .into_iter()
                .map(|p| DevicePlacement {
                    peer_fingerprint: p.fingerprint,
                    origin_in_workspace: (p.origin_x, p.origin_y),
                })
                .collect(),
        };
        config.validate()?;
        Ok(config)
    }
}

/// In-memory workspace + live topologies + derived routing.
#[derive(Debug, Default)]
pub struct WorkspaceState {
    path: PathBuf,
    /// Local authenticated fingerprint (this device).
    local_fingerprint: String,
    config: Option<WorkspaceConfig>,
    topologies: BTreeMap<PeerFingerprint, LocalTopology>,
    snapshot: Option<RoutingSnapshot>,
    /// Chunk reassembly keyed by publisher fingerprint + revision.
    pending_chunks: HashMap<(String, u64), Vec<(u8, u8, Vec<u8>)>>,
    /// Last accepted revision we have ACKed.
    last_acked_revision: Option<u64>,
}

impl WorkspaceState {
    pub fn new(config_dir: &Path, local_fingerprint: String) -> Self {
        Self {
            path: config_dir.join(WORKSPACE_FILE_NAME),
            local_fingerprint,
            ..Default::default()
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn local_fingerprint(&self) -> &str {
        &self.local_fingerprint
    }

    pub fn config(&self) -> Option<&WorkspaceConfig> {
        self.config.as_ref()
    }

    pub fn snapshot(&self) -> Option<&RoutingSnapshot> {
        self.snapshot.as_ref()
    }

    pub fn is_authority(&self) -> bool {
        self.config
            .as_ref()
            .is_some_and(|c| c.authority_fingerprint == self.local_fingerprint)
    }

    pub fn load_from_disk(&mut self) -> Result<(), WorkspaceStateError> {
        if !self.path.exists() {
            self.config = None;
            self.rebuild_snapshot()?;
            return Ok(());
        }
        let text = fs::read_to_string(&self.path)?;
        let toml: TomlWorkspace = toml::from_str(&text)?;
        let config = WorkspaceConfig::try_from(toml)?;
        self.config = Some(config);
        self.rebuild_snapshot()?;
        Ok(())
    }

    pub fn save_to_disk(&self) -> Result<(), WorkspaceStateError> {
        let Some(config) = &self.config else {
            if self.path.exists() {
                fs::remove_file(&self.path)?;
            }
            return Ok(());
        };
        let toml = TomlWorkspace::from(config);
        let text = toml::to_string_pretty(&toml)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.path, text)?;
        Ok(())
    }

    /// Authority applies a validated draft. Followers must use [`accept_remote`].
    pub fn apply_local(&mut self, mut config: WorkspaceConfig) -> Result<(), WorkspaceStateError> {
        config.validate()?;
        if config.authority_fingerprint != self.local_fingerprint {
            return Err(WorkspaceStateError::Unauthorized(
                config.authority_fingerprint.clone(),
            ));
        }
        if let Some(current) = &self.config {
            if config.workspace_id != current.workspace_id {
                // Explicit new workspace id is allowed from authority.
            }
            if config.revision <= current.revision {
                config.revision = current.revision.saturating_add(1);
            }
        } else if config.revision == 0 {
            config.revision = 1;
        }
        // Validate routing against currently known topologies (offline peers ok).
        let _ = derive_routes(&config, &self.topologies)?;
        self.config = Some(config);
        self.rebuild_snapshot()?;
        self.save_to_disk()?;
        Ok(())
    }

    /// Create an initial empty workspace with this device as authority.
    pub fn ensure_authority_workspace(
        &mut self,
        workspace_id: impl Into<String>,
    ) -> Result<(), WorkspaceStateError> {
        if self.config.is_some() {
            return Ok(());
        }
        let config = WorkspaceConfig {
            workspace_id: workspace_id.into(),
            revision: 1,
            authority_fingerprint: self.local_fingerprint.clone(),
            placements: vec![DevicePlacement {
                peer_fingerprint: self.local_fingerprint.clone(),
                origin_in_workspace: (0, 0),
            }],
        };
        self.apply_local(config)
    }

    pub fn set_topology(
        &mut self,
        peer: PeerFingerprint,
        topology: LocalTopology,
    ) -> Result<(), WorkspaceStateError> {
        self.topologies.insert(peer, topology);
        self.rebuild_snapshot()?;
        Ok(())
    }

    pub fn remove_topology(&mut self, peer: &str) -> Result<(), WorkspaceStateError> {
        self.topologies.remove(peer);
        self.rebuild_snapshot()?;
        Ok(())
    }

    pub fn rebuild_snapshot(&mut self) -> Result<(), WorkspaceStateError> {
        let Some(config) = &self.config else {
            self.snapshot = None;
            return Ok(());
        };
        match derive_routes(config, &self.topologies) {
            Ok(snap) => self.snapshot = Some(snap),
            Err(WorkspaceError::OverlappingDevices(_, _) | WorkspaceError::AmbiguousRoutes) => {
                // Keep last valid active snapshot; surface error to caller.
                return Err(WorkspaceStateError::Geometry(
                    WorkspaceError::AmbiguousRoutes,
                ));
            }
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }

    /// Encode publish chunks for peers (authority only).
    pub fn publish_chunks(&self) -> Result<Vec<ProtoEvent>, WorkspaceStateError> {
        let config = self
            .config
            .as_ref()
            .ok_or(WorkspaceStateError::InvalidPayload)?;
        if config.authority_fingerprint != self.local_fingerprint {
            return Err(WorkspaceStateError::Unauthorized(
                self.local_fingerprint.clone(),
            ));
        }
        let payload =
            encode_workspace_payload(config).map_err(|_| WorkspaceStateError::InvalidPayload)?;
        workspace_chunks(config.revision, &payload).map_err(|_| WorkspaceStateError::InvalidPayload)
    }

    /// Ingest one chunk from an authenticated peer. Returns `Some(revision)` when
    /// a full snapshot was accepted and persisted.
    pub fn ingest_chunk(
        &mut self,
        publisher_fingerprint: &str,
        revision: u64,
        index: u8,
        count: u8,
        data: Vec<u8>,
    ) -> Result<Option<u64>, WorkspaceStateError> {
        let expected_authority = self
            .config
            .as_ref()
            .map(|c| c.authority_fingerprint.as_str())
            .unwrap_or(publisher_fingerprint);
        // First publish or matching authority only.
        if publisher_fingerprint != expected_authority
            && self
                .config
                .as_ref()
                .is_some_and(|c| c.authority_fingerprint != publisher_fingerprint)
        {
            return Err(WorkspaceStateError::Unauthorized(
                publisher_fingerprint.to_string(),
            ));
        }
        if let Some(current) = &self.config {
            if revision < current.revision {
                return Err(WorkspaceStateError::StaleRevision {
                    got: revision,
                    have: current.revision,
                });
            }
            if revision == current.revision {
                // Duplicate publish of current revision — ACK without rewrite.
                return Ok(Some(revision));
            }
        }

        let key = (publisher_fingerprint.to_string(), revision);
        let entry = self.pending_chunks.entry(key.clone()).or_default();
        entry.push((index, count, data));
        let payload = match try_reassemble_workspace(entry)
            .map_err(|_| WorkspaceStateError::InvalidPayload)?
        {
            Some(p) => p,
            None => return Ok(None),
        };
        self.pending_chunks.remove(&key);
        let config =
            decode_workspace_payload(&payload).map_err(|_| WorkspaceStateError::InvalidPayload)?;
        if config.authority_fingerprint != publisher_fingerprint {
            return Err(WorkspaceStateError::Unauthorized(
                publisher_fingerprint.to_string(),
            ));
        }
        if config.revision != revision {
            return Err(WorkspaceStateError::InvalidPayload);
        }
        // Validate geometry; reject without clobbering last good config.
        derive_routes(&config, &self.topologies)?;
        self.config = Some(config);
        self.rebuild_snapshot()?;
        self.save_to_disk()?;
        self.last_acked_revision = Some(revision);
        Ok(Some(revision))
    }

    pub fn peer_supports_shared(capabilities: u32) -> bool {
        capabilities & mousehop_proto::CAP_SHARED_WORKSPACE != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use input_event::display::DisplayRect;
    use input_event::workspace::{DisplayId, LocalDisplay};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_workspace_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("mousehop-ws-{label}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn topo_one(name: &str, w: u32, h: u32) -> LocalTopology {
        LocalTopology::try_new(
            vec![LocalDisplay {
                id: DisplayId::new(name).unwrap(),
                rect: DisplayRect::new(0, 0, w, h).unwrap(),
                scale: None,
                rotation: None,
            }],
            1,
            1,
        )
        .unwrap()
    }

    #[test]
    fn persist_and_reload() {
        let dir = temp_workspace_dir("persist");
        let mut state = WorkspaceState::new(&dir, "auth".into());
        state
            .apply_local(WorkspaceConfig {
                workspace_id: "ws".into(),
                revision: 1,
                authority_fingerprint: "auth".into(),
                placements: vec![DevicePlacement {
                    peer_fingerprint: "auth".into(),
                    origin_in_workspace: (0, 0),
                }],
            })
            .unwrap();
        let mut reloaded = WorkspaceState::new(&dir, "auth".into());
        reloaded.load_from_disk().unwrap();
        assert_eq!(reloaded.config().unwrap().revision, 1);
        assert!(reloaded.is_authority());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reject_unauthorized_publisher() {
        let dir = temp_workspace_dir("unauth");
        let mut state = WorkspaceState::new(&dir, "auth".into());
        state
            .apply_local(WorkspaceConfig {
                workspace_id: "ws".into(),
                revision: 1,
                authority_fingerprint: "auth".into(),
                placements: vec![],
            })
            .unwrap();
        let evil_dir = temp_workspace_dir("evil");
        let chunks = {
            let mut evil = WorkspaceState::new(&evil_dir, "evil".into());
            evil.apply_local(WorkspaceConfig {
                workspace_id: "ws".into(),
                revision: 2,
                authority_fingerprint: "evil".into(),
                placements: vec![],
            })
            .unwrap();
            let payload = encode_workspace_payload(evil.config().unwrap()).unwrap();
            workspace_chunks(2, &payload).unwrap()
        };
        for chunk in chunks {
            match chunk {
                ProtoEvent::WorkspaceChunk {
                    revision,
                    index,
                    count,
                    data,
                } => {
                    let err = state
                        .ingest_chunk("evil", revision, index, count, data)
                        .unwrap_err();
                    assert!(matches!(err, WorkspaceStateError::Unauthorized(_)));
                }
                _ => panic!("expected chunk"),
            }
        }
        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(evil_dir);
    }

    #[test]
    fn follower_accepts_authority_chunks_and_routes() {
        let dir = temp_workspace_dir("auth-pub");
        let mut authority = WorkspaceState::new(&dir, "auth".into());
        authority
            .set_topology("auth".into(), topo_one("a", 1000, 1000))
            .unwrap();
        authority
            .set_topology("peer".into(), topo_one("b", 1000, 1000))
            .unwrap();
        authority
            .apply_local(WorkspaceConfig {
                workspace_id: "ws".into(),
                revision: 1,
                authority_fingerprint: "auth".into(),
                placements: vec![
                    DevicePlacement {
                        peer_fingerprint: "auth".into(),
                        origin_in_workspace: (0, 0),
                    },
                    DevicePlacement {
                        peer_fingerprint: "peer".into(),
                        origin_in_workspace: (1000, 0),
                    },
                ],
            })
            .unwrap();
        let chunks = authority.publish_chunks().unwrap();

        let peer_dir = temp_workspace_dir("follower");
        let mut follower = WorkspaceState::new(&peer_dir, "peer".into());
        follower
            .set_topology("auth".into(), topo_one("a", 1000, 1000))
            .unwrap();
        follower
            .set_topology("peer".into(), topo_one("b", 1000, 1000))
            .unwrap();
        let mut accepted = None;
        for chunk in chunks {
            match chunk {
                ProtoEvent::WorkspaceChunk {
                    revision,
                    index,
                    count,
                    data,
                } => {
                    accepted = follower
                        .ingest_chunk("auth", revision, index, count, data)
                        .unwrap();
                }
                _ => panic!("chunk"),
            }
        }
        assert_eq!(accepted, Some(1));
        assert!(!follower.is_authority());
        let snap = follower.snapshot().unwrap();
        assert!(
            snap.lookup(
                "auth",
                (999, 100),
                input_event::workspace::OutboundDir::Right
            )
            .is_some()
        );
        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(peer_dir);
    }

    #[test]
    fn stale_revision_rejected() {
        let dir = temp_workspace_dir("stale");
        let mut state = WorkspaceState::new(&dir, "auth".into());
        state
            .apply_local(WorkspaceConfig {
                workspace_id: "ws".into(),
                revision: 5,
                authority_fingerprint: "auth".into(),
                placements: vec![],
            })
            .unwrap();
        let err = state
            .ingest_chunk("auth", 4, 0, 1, vec![1, 2, 3])
            .unwrap_err();
        assert!(matches!(
            err,
            WorkspaceStateError::StaleRevision { got: 4, have: 5 }
        ));
        let _ = fs::remove_dir_all(dir);
    }
}
