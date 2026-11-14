//! Seed shared-workspace placements from legacy outgoing clients.

use input_event::workspace::{DevicePlacement, LocalTopology};
use mousehop_ipc::Position;

pub const PLACEHOLDER_W: i32 = 1920;
pub const PLACEHOLDER_H: i32 = 1080;
/// Gap when stacking multiple peers on the same legacy edge.
pub const EDGE_STACK_GAP: i32 = 1200;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Aabb {
    pub min_x: i32,
    pub min_y: i32,
    pub max_x: i32,
    pub max_y: i32,
}

impl Aabb {
    pub fn width(self) -> i32 {
        self.max_x.saturating_sub(self.min_x)
    }

    pub fn height(self) -> i32 {
        self.max_y.saturating_sub(self.min_y)
    }
}

/// Axis-aligned bounds of a topology, or a placeholder desktop if empty.
pub fn topology_aabb(topo: Option<&LocalTopology>) -> Aabb {
    let Some(topo) = topo else {
        return Aabb {
            min_x: 0,
            min_y: 0,
            max_x: PLACEHOLDER_W,
            max_y: PLACEHOLDER_H,
        };
    };
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for d in &topo.displays {
        let (x, y) = d.rect.origin();
        let (w, h) = d.rect.size();
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x + w as i32);
        max_y = max_y.max(y + h as i32);
    }
    if min_x == i32::MAX {
        return Aabb {
            min_x: 0,
            min_y: 0,
            max_x: PLACEHOLDER_W,
            max_y: PLACEHOLDER_H,
        };
    }
    Aabb {
        min_x,
        min_y,
        max_x,
        max_y,
    }
}

/// Place a peer relative to the local AABB using legacy Position.
///
/// `edge_index` stacks multiple peers that share the same edge
/// (`0` = first on that edge).
pub fn origin_for_legacy_position(
    local: Aabb,
    peer_w: i32,
    peer_h: i32,
    position: Position,
    edge_index: u32,
) -> (i32, i32) {
    let stack = (edge_index as i32).saturating_mul(EDGE_STACK_GAP);
    match position {
        Position::Right => (local.max_x, local.min_y + stack),
        Position::Left => (local.min_x - peer_w, local.min_y + stack),
        Position::Bottom => (local.min_x + stack, local.max_y),
        Position::Top => (local.min_x + stack, local.min_y - peer_h),
    }
}

/// Hostname-first label for a workspace device.
pub fn resolve_workspace_label(
    fingerprint: &str,
    local_fingerprint: &str,
    local_hostname: Option<&str>,
    outgoing_hostname: Option<&str>,
    authorized_description: Option<&str>,
    authorized_last_hostname: Option<&str>,
) -> String {
    if fingerprint == local_fingerprint {
        if let Some(h) = local_hostname.filter(|s| !s.is_empty()) {
            return h.to_string();
        }
    }
    if let Some(h) = outgoing_hostname.filter(|s| !s.is_empty()) {
        return h.to_string();
    }
    if let Some(d) = authorized_description.filter(|s| !s.is_empty()) {
        return d.to_string();
    }
    if let Some(h) = authorized_last_hostname.filter(|s| !s.is_empty()) {
        return h.to_string();
    }
    fingerprint.chars().take(8).collect()
}

/// One outgoing client eligible for workspace seeding.
#[derive(Clone, Debug)]
pub struct LegacyOutgoingPeer {
    pub fingerprint: String,
    pub position: Position,
    pub peer_topology: Option<LocalTopology>,
}

/// Build placements: local at (0,0) plus known-fp outgoing peers from Position.
pub fn seed_legacy_placements(
    local_fingerprint: &str,
    local_topology: Option<&LocalTopology>,
    outgoing: &[LegacyOutgoingPeer],
) -> Vec<DevicePlacement> {
    let mut placements = Vec::new();
    if local_topology.is_some() || !outgoing.is_empty() {
        placements.push(DevicePlacement {
            peer_fingerprint: local_fingerprint.to_string(),
            origin_in_workspace: (0, 0),
        });
    }
    let local = topology_aabb(local_topology);
    let mut edge_counts: [u32; 4] = [0; 4];
    for peer in outgoing {
        if peer.fingerprint == local_fingerprint {
            continue;
        }
        if placements
            .iter()
            .any(|p| p.peer_fingerprint == peer.fingerprint)
        {
            continue;
        }
        let peer_aabb = topology_aabb(peer.peer_topology.as_ref());
        let edge_i = match peer.position {
            Position::Left => 0,
            Position::Right => 1,
            Position::Top => 2,
            Position::Bottom => 3,
        };
        let idx = edge_counts[edge_i];
        edge_counts[edge_i] = edge_counts[edge_i].saturating_add(1);
        let origin = origin_for_legacy_position(
            local,
            peer_aabb.width(),
            peer_aabb.height(),
            peer.position,
            idx,
        );
        placements.push(DevicePlacement {
            peer_fingerprint: peer.fingerprint.clone(),
            origin_in_workspace: origin,
        });
    }
    placements
}

#[cfg(test)]
mod tests {
    use super::*;
    use input_event::display::DisplayRect;
    use input_event::workspace::{DisplayId, LocalDisplay};

    fn topo_one(w: u32, h: u32) -> LocalTopology {
        LocalTopology::try_new(
            vec![LocalDisplay {
                id: DisplayId::new("d0").unwrap(),
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
    fn origin_right_left_top_bottom() {
        let local = Aabb {
            min_x: 0,
            min_y: 0,
            max_x: 3840,
            max_y: 2160,
        };
        assert_eq!(
            origin_for_legacy_position(local, 1920, 1080, Position::Right, 0),
            (3840, 0)
        );
        assert_eq!(
            origin_for_legacy_position(local, 1920, 1080, Position::Left, 0),
            (-1920, 0)
        );
        assert_eq!(
            origin_for_legacy_position(local, 1920, 1080, Position::Bottom, 0),
            (0, 2160)
        );
        assert_eq!(
            origin_for_legacy_position(local, 1920, 1080, Position::Top, 0),
            (0, -1080)
        );
    }

    #[test]
    fn origin_stacks_same_edge() {
        let local = Aabb {
            min_x: 0,
            min_y: 0,
            max_x: 1920,
            max_y: 1080,
        };
        assert_eq!(
            origin_for_legacy_position(local, 1920, 1080, Position::Right, 0),
            (1920, 0)
        );
        assert_eq!(
            origin_for_legacy_position(local, 1920, 1080, Position::Right, 1),
            (1920, EDGE_STACK_GAP)
        );
    }

    #[test]
    fn seed_includes_local_and_outgoing() {
        let local = topo_one(3840, 2160);
        let peers = vec![LegacyOutgoingPeer {
            fingerprint: "peer-fp".into(),
            position: Position::Right,
            peer_topology: None,
        }];
        let placements = seed_legacy_placements("local-fp", Some(&local), &peers);
        assert_eq!(placements.len(), 2);
        assert_eq!(placements[0].peer_fingerprint, "local-fp");
        assert_eq!(placements[0].origin_in_workspace, (0, 0));
        assert_eq!(placements[1].peer_fingerprint, "peer-fp");
        assert_eq!(placements[1].origin_in_workspace, (3840, 0));
    }

    #[test]
    fn label_priority() {
        assert_eq!(
            resolve_workspace_label("local", "local", Some("DeskPC"), None, None, None),
            "DeskPC"
        );
        assert_eq!(
            resolve_workspace_label(
                "peer",
                "local",
                Some("DeskPC"),
                Some("MacBookPro.lan"),
                Some("ignored"),
                None
            ),
            "MacBookPro.lan"
        );
        assert_eq!(
            resolve_workspace_label("peer", "local", None, None, Some("Office Mac"), None),
            "Office Mac"
        );
        assert_eq!(
            resolve_workspace_label("peer", "local", None, None, None, Some("mac.local")),
            "mac.local"
        );
        assert_eq!(
            resolve_workspace_label("c5:4f:ce:4a:bb", "local", None, None, None, None),
            "c5:4f:ce"
        );
    }
}
