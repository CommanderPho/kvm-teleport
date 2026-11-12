//! Shared spatial workspace geometry and cursor routing.
//!
//! # Coordinate policy
//!
//! Workspace coordinates are **logical points** as reported by each platform's
//! capture and warp APIs (macOS Quartz points, Windows virtual-screen pixels).
//! Device placements store the workspace position of each device's local
//! origin; each monitor keeps the relative arrangement reported by that OS.
//! Physical millimetre calibration is out of scope. Mixed-DPI peers therefore
//! align in logical space, not physical size.
//!
//! # Identity
//!
//! [`DisplayId`] values are opaque strings. Prefer OS-stable identifiers when
//! available. Fallback IDs must not use enumeration order alone; callers should
//! derive a content-based token (adapter name + geometry) and bump topology
//! generation on hotplug so stale IDs are never silently reused for a different
//! monitor.
//!
//! # Boundary segments vs outer envelopes
//!
//! [`boundary_segments`] enumerates every half-open edge interval of a display
//! that is **not** covered by another same-device display. That includes
//! inward-facing edges in U-shaped arrangements. This is distinct from
//! [`crate::display::DisplayLayout::exposed_segments`], which only builds the
//! directional outer contour used by legacy KVM barriers.

use crate::display::{DisplayEdge, DisplayRect};
use std::collections::BTreeMap;
use thiserror::Error;

/// Opaque stable-ish display identifier within one device's topology.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DisplayId(String);

impl DisplayId {
    /// Creates a display id from a non-empty string.
    pub fn new(id: impl Into<String>) -> Option<Self> {
        let id = id.into();
        if id.is_empty() || id.len() > MAX_DISPLAY_ID_LEN {
            return None;
        }
        Some(Self(id))
    }

    /// Returns the raw identifier bytes as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DisplayId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Maximum accepted display-id length (wire and persistence bound).
pub const MAX_DISPLAY_ID_LEN: usize = 64;

/// Maximum displays per device topology.
pub const MAX_LOCAL_DISPLAYS: usize = 64;

/// Maximum devices in one workspace.
pub const MAX_WORKSPACE_DEVICES: usize = 32;

/// Inset applied just inside the destination edge after a crossing.
pub const LANDING_INSET: i32 = 1;

/// Authenticated peer fingerprint used as device identity.
pub type PeerFingerprint = String;

/// One monitor in a device's local coordinate system.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalDisplay {
    /// Stable-ish identifier within this device.
    pub id: DisplayId,
    /// Rectangle in the coordinate system used by capture/warp on this OS.
    pub rect: DisplayRect,
    /// Optional logical scale (e.g. 2.0 for Retina). Informational only for routing.
    pub scale: Option<f32>,
    /// Optional rotation in degrees. Informational; rect already reflects orientation.
    pub rotation: Option<u16>,
}

/// Live display topology for one device.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LocalTopology {
    /// Displays attached to this device.
    pub displays: Vec<LocalDisplay>,
    /// Session epoch; changes when the capture/emulation backend restarts.
    pub epoch: u64,
    /// Monotonic generation within an epoch; bumps on hotplug / layout change.
    pub generation: u32,
}

impl LocalTopology {
    /// Builds a topology, rejecting oversized or duplicate ids.
    pub fn try_new(
        displays: Vec<LocalDisplay>,
        epoch: u64,
        generation: u32,
    ) -> Result<Self, WorkspaceError> {
        if displays.len() > MAX_LOCAL_DISPLAYS {
            return Err(WorkspaceError::TooManyDisplays(displays.len()));
        }
        let mut seen = BTreeMap::new();
        for display in &displays {
            if seen.insert(display.id.clone(), ()).is_some() {
                return Err(WorkspaceError::DuplicateDisplayId(display.id.clone()));
            }
        }
        Ok(Self {
            displays,
            epoch,
            generation,
        })
    }

    /// Returns the display with the given id.
    pub fn display(&self, id: &DisplayId) -> Option<&LocalDisplay> {
        self.displays.iter().find(|d| d.id == *id)
    }
}

/// Placement of one device's local origin inside the shared workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevicePlacement {
    /// Authenticated peer fingerprint.
    pub peer_fingerprint: PeerFingerprint,
    /// Workspace coordinates of this device's local `(0, 0)`.
    pub origin_in_workspace: (i32, i32),
}

/// Authoritative shared workspace configuration (persisted / published).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceConfig {
    /// Persistent workspace identifier.
    pub workspace_id: String,
    /// Monotonic configuration revision (separate from live topology generation).
    pub revision: u64,
    /// Only this fingerprint may publish updates.
    pub authority_fingerprint: PeerFingerprint,
    /// Placements for known devices (including offline peers).
    pub placements: Vec<DevicePlacement>,
}

impl WorkspaceConfig {
    /// Validates bounds and uniqueness.
    pub fn validate(&self) -> Result<(), WorkspaceError> {
        if self.workspace_id.is_empty() || self.workspace_id.len() > 128 {
            return Err(WorkspaceError::InvalidWorkspaceId);
        }
        if self.authority_fingerprint.is_empty() {
            return Err(WorkspaceError::InvalidAuthority);
        }
        if self.placements.len() > MAX_WORKSPACE_DEVICES {
            return Err(WorkspaceError::TooManyDevices(self.placements.len()));
        }
        let mut seen = BTreeMap::new();
        for placement in &self.placements {
            if placement.peer_fingerprint.is_empty() {
                return Err(WorkspaceError::InvalidAuthority);
            }
            if seen
                .insert(placement.peer_fingerprint.clone(), ())
                .is_some()
            {
                return Err(WorkspaceError::DuplicatePeer(
                    placement.peer_fingerprint.clone(),
                ));
            }
        }
        Ok(())
    }
}

/// A half-open boundary interval on one display edge (may be inward-facing).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundarySegment {
    pub display_id: DisplayId,
    pub edge: DisplayEdge,
    /// Inclusive cross-axis start.
    pub start: i32,
    /// Exclusive cross-axis end.
    pub end: i32,
    /// Inclusive on-axis coordinate of the last in-layout pixel.
    pub coordinate: i32,
}

impl BoundarySegment {
    pub fn len(&self) -> u32 {
        if self.start >= self.end {
            0
        } else {
            (i64::from(self.end) - i64::from(self.start)) as u32
        }
    }

    pub fn contains_cross(&self, cross: i32) -> bool {
        cross >= self.start && cross < self.end
    }
}

/// Outbound motion direction used for crossing detection.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OutboundDir {
    Left,
    Right,
    Up,
    Down,
}

impl OutboundDir {
    pub fn edge(self) -> DisplayEdge {
        match self {
            Self::Left => DisplayEdge::Left,
            Self::Right => DisplayEdge::Right,
            Self::Up => DisplayEdge::Top,
            Self::Down => DisplayEdge::Bottom,
        }
    }

    pub fn from_delta(dx: f64, dy: f64) -> Option<Self> {
        if !dx.is_finite() || !dy.is_finite() {
            return None;
        }
        let ax = dx.abs();
        let ay = dy.abs();
        if ax == 0.0 && ay == 0.0 {
            return None;
        }
        // Prefer the dominant axis; ties prefer horizontal then left/up deterministically.
        if ax >= ay {
            if dx < 0.0 {
                Some(Self::Left)
            } else {
                Some(Self::Right)
            }
        } else if dy < 0.0 {
            Some(Self::Up)
        } else {
            Some(Self::Down)
        }
    }
}

/// One traversable adjacency interval between two devices.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteInterval {
    pub src_peer: PeerFingerprint,
    pub src_display: DisplayId,
    pub src_edge: DisplayEdge,
    /// Inclusive/exclusive cross-axis range on the **source** edge (workspace).
    pub src_start: i32,
    pub src_end: i32,
    pub dst_peer: PeerFingerprint,
    pub dst_display: DisplayId,
    pub dst_edge: DisplayEdge,
    pub dst_start: i32,
    pub dst_end: i32,
}

/// Cursor landing after a successful spatial crossing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Landing {
    pub dst_peer: PeerFingerprint,
    pub dst_display: DisplayId,
    /// Destination local coordinates (device space), already inset.
    pub local_position: (i32, i32),
    /// Workspace revision that produced this route.
    pub workspace_revision: u64,
    /// Destination topology generation observed when the snapshot was built.
    pub dst_topology_generation: u32,
}

/// Immutable validated routing table derived from workspace + live topologies.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoutingSnapshot {
    pub workspace_id: String,
    pub workspace_revision: u64,
    routes: Vec<RouteInterval>,
    /// Workspace-space rectangles keyed by (peer, display).
    displays: BTreeMap<(PeerFingerprint, DisplayId), PlacedDisplay>,
}

#[derive(Clone, Debug, PartialEq)]
struct PlacedDisplay {
    peer: PeerFingerprint,
    display_id: DisplayId,
    /// Rectangle in workspace coordinates.
    workspace_rect: DisplayRect,
    /// Local origin offset used to convert workspace → local.
    local_origin: (i32, i32),
    topology_generation: u32,
    scale: Option<f32>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum WorkspaceError {
    #[error("invalid workspace id")]
    InvalidWorkspaceId,
    #[error("invalid authority fingerprint")]
    InvalidAuthority,
    #[error("too many devices: {0}")]
    TooManyDevices(usize),
    #[error("too many displays: {0}")]
    TooManyDisplays(usize),
    #[error("duplicate display id: {0}")]
    DuplicateDisplayId(DisplayId),
    #[error("duplicate peer placement: {0}")]
    DuplicatePeer(String),
    #[error("overlapping workspace displays between {0} and {1}")]
    OverlappingDevices(String, String),
    #[error("missing topology for peer {0}")]
    MissingTopology(String),
    #[error("invalid placement translation")]
    InvalidTranslation,
    #[error("ambiguous overlapping routes")]
    AmbiguousRoutes,
}

/// Enumerates full per-display boundary segments, including inward-facing edges.
///
/// An edge interval is omitted only when another **same-device** display covers
/// the adjacent pixel just outside that edge (internal seam).
pub fn boundary_segments(topology: &LocalTopology) -> Vec<BoundarySegment> {
    let mut segments = Vec::new();
    for display in &topology.displays {
        for edge in [
            DisplayEdge::Left,
            DisplayEdge::Right,
            DisplayEdge::Top,
            DisplayEdge::Bottom,
        ] {
            let (cross_start, cross_end) = display.rect.cross_range(edge);
            if cross_start >= cross_end {
                continue;
            }
            let mut cuts = vec![cross_start, cross_end];
            // Subtract intervals covered by neighboring same-device displays.
            for other in &topology.displays {
                if other.id == display.id {
                    continue;
                }
                if let Some((cover_start, cover_end)) =
                    covered_cross_interval(display.rect, other.rect, edge)
                {
                    cuts.push(cover_start);
                    cuts.push(cover_end);
                }
            }
            cuts.sort_unstable();
            cuts.dedup();
            let coordinate = display.rect.edge_coordinate(edge);
            for pair in cuts.windows(2) {
                let start = pair[0];
                let end = pair[1];
                if start >= end {
                    continue;
                }
                if start < cross_start || end > cross_end {
                    continue;
                }
                // Skip if fully covered by a neighbor.
                let covered = topology.displays.iter().any(|other| {
                    other.id != display.id
                        && covered_cross_interval(display.rect, other.rect, edge)
                            .is_some_and(|(cs, ce)| cs <= start && ce >= end)
                });
                if covered {
                    continue;
                }
                segments.push(BoundarySegment {
                    display_id: display.id.clone(),
                    edge,
                    start,
                    end,
                    coordinate,
                });
            }
        }
    }
    segments
}

fn covered_cross_interval(
    self_rect: DisplayRect,
    other: DisplayRect,
    edge: DisplayEdge,
) -> Option<(i32, i32)> {
    // Neighbor must touch just outside this edge and overlap on the cross axis.
    let touches = match edge {
        DisplayEdge::Left => other.right() == self_rect.x(),
        DisplayEdge::Right => other.x() == self_rect.right(),
        DisplayEdge::Top => other.bottom() == self_rect.y(),
        DisplayEdge::Bottom => other.y() == self_rect.bottom(),
    };
    if !touches {
        return None;
    }
    let (a0, a1) = self_rect.cross_range(edge);
    let (b0, b1) = other.cross_range(edge);
    let start = a0.max(b0);
    let end = a1.min(b1);
    (start < end).then_some((start, end))
}

/// Derives an immutable routing snapshot from authoritative placements + live topologies.
pub fn derive_routes(
    workspace: &WorkspaceConfig,
    topologies: &BTreeMap<PeerFingerprint, LocalTopology>,
) -> Result<RoutingSnapshot, WorkspaceError> {
    workspace.validate()?;

    let mut placed: BTreeMap<(PeerFingerprint, DisplayId), PlacedDisplay> = BTreeMap::new();
    for placement in &workspace.placements {
        let Some(topology) = topologies.get(&placement.peer_fingerprint) else {
            // Offline peers keep their placement but contribute no live routes.
            continue;
        };
        for display in &topology.displays {
            let workspace_rect = display
                .rect
                .translated(
                    placement.origin_in_workspace.0,
                    placement.origin_in_workspace.1,
                )
                .ok_or(WorkspaceError::InvalidTranslation)?;
            let key = (placement.peer_fingerprint.clone(), display.id.clone());
            placed.insert(
                key,
                PlacedDisplay {
                    peer: placement.peer_fingerprint.clone(),
                    display_id: display.id.clone(),
                    workspace_rect,
                    local_origin: placement.origin_in_workspace,
                    topology_generation: topology.generation,
                    scale: display.scale,
                },
            );
        }
    }

    // Reject cross-device overlaps.
    let entries: Vec<_> = placed.values().collect();
    for i in 0..entries.len() {
        for j in (i + 1)..entries.len() {
            if entries[i].peer == entries[j].peer {
                continue;
            }
            if entries[i]
                .workspace_rect
                .overlaps(entries[j].workspace_rect)
            {
                return Err(WorkspaceError::OverlappingDevices(
                    entries[i].peer.clone(),
                    entries[j].peer.clone(),
                ));
            }
        }
    }

    let mut routes = Vec::new();
    for a in placed.values() {
        for b in placed.values() {
            if a.peer == b.peer {
                continue;
            }
            push_adjacency_routes(a, b, &mut routes);
        }
    }

    // Detect ambiguous overlapping source intervals to different destinations.
    detect_ambiguous_routes(&routes)?;

    Ok(RoutingSnapshot {
        workspace_id: workspace.workspace_id.clone(),
        workspace_revision: workspace.revision,
        routes,
        displays: placed,
    })
}

fn push_adjacency_routes(a: &PlacedDisplay, b: &PlacedDisplay, routes: &mut Vec<RouteInterval>) {
    // a.Right ↔ b.Left when a.right == b.x and y ranges overlap.
    try_edge_pair(
        a,
        b,
        DisplayEdge::Right,
        DisplayEdge::Left,
        a.workspace_rect.right() == b.workspace_rect.x(),
        routes,
    );
    try_edge_pair(
        a,
        b,
        DisplayEdge::Left,
        DisplayEdge::Right,
        a.workspace_rect.x() == b.workspace_rect.right(),
        routes,
    );
    try_edge_pair(
        a,
        b,
        DisplayEdge::Bottom,
        DisplayEdge::Top,
        a.workspace_rect.bottom() == b.workspace_rect.y(),
        routes,
    );
    try_edge_pair(
        a,
        b,
        DisplayEdge::Top,
        DisplayEdge::Bottom,
        a.workspace_rect.y() == b.workspace_rect.bottom(),
        routes,
    );
}

fn try_edge_pair(
    a: &PlacedDisplay,
    b: &PlacedDisplay,
    a_edge: DisplayEdge,
    b_edge: DisplayEdge,
    adjacent: bool,
    routes: &mut Vec<RouteInterval>,
) {
    if !adjacent {
        return;
    }
    let (a0, a1) = a.workspace_rect.cross_range(a_edge);
    let (b0, b1) = b.workspace_rect.cross_range(b_edge);
    let start = a0.max(b0);
    let end = a1.min(b1);
    if start >= end {
        return;
    }
    routes.push(RouteInterval {
        src_peer: a.peer.clone(),
        src_display: a.display_id.clone(),
        src_edge: a_edge,
        src_start: start,
        src_end: end,
        dst_peer: b.peer.clone(),
        dst_display: b.display_id.clone(),
        dst_edge: b_edge,
        dst_start: start,
        dst_end: end,
    });
}

fn detect_ambiguous_routes(routes: &[RouteInterval]) -> Result<(), WorkspaceError> {
    for i in 0..routes.len() {
        for j in (i + 1)..routes.len() {
            let a = &routes[i];
            let b = &routes[j];
            if a.src_peer != b.src_peer
                || a.src_display != b.src_display
                || a.src_edge != b.src_edge
            {
                continue;
            }
            if a.dst_peer == b.dst_peer && a.dst_display == b.dst_display {
                continue;
            }
            let overlap_start = a.src_start.max(b.src_start);
            let overlap_end = a.src_end.min(b.src_end);
            if overlap_start < overlap_end {
                return Err(WorkspaceError::AmbiguousRoutes);
            }
        }
    }
    Ok(())
}

impl RoutingSnapshot {
    /// Returns all route intervals (for UI highlighting / capture arming).
    pub fn routes(&self) -> &[RouteInterval] {
        &self.routes
    }

    /// Returns routes leaving `src_peer`.
    pub fn routes_from<'a>(
        &'a self,
        src_peer: &'a str,
    ) -> impl Iterator<Item = &'a RouteInterval> + 'a {
        self.routes.iter().filter(move |r| r.src_peer == src_peer)
    }

    /// Looks up a landing for an outbound crossing at `workspace_point`.
    pub fn lookup(
        &self,
        src_peer: &str,
        workspace_point: (i32, i32),
        dir: OutboundDir,
    ) -> Option<Landing> {
        let edge = dir.edge();
        let cross = match edge {
            DisplayEdge::Left | DisplayEdge::Right => workspace_point.1,
            DisplayEdge::Top | DisplayEdge::Bottom => workspace_point.0,
        };
        let on_axis = match edge {
            DisplayEdge::Left | DisplayEdge::Right => workspace_point.0,
            DisplayEdge::Top | DisplayEdge::Bottom => workspace_point.1,
        };

        let mut candidates: Vec<&RouteInterval> = self
            .routes
            .iter()
            .filter(|r| {
                r.src_peer == src_peer
                    && r.src_edge == edge
                    && cross >= r.src_start
                    && cross < r.src_end
            })
            .collect();
        if candidates.is_empty() {
            return None;
        }
        // Prefer the route whose source edge coordinate is nearest to on_axis.
        candidates.sort_by(|a, b| {
            let a_coord = self
                .displays
                .get(&(a.src_peer.clone(), a.src_display.clone()))
                .map(|d| d.workspace_rect.edge_coordinate(edge))
                .unwrap_or(0);
            let b_coord = self
                .displays
                .get(&(b.src_peer.clone(), b.src_display.clone()))
                .map(|d| d.workspace_rect.edge_coordinate(edge))
                .unwrap_or(0);
            let a_dist = (a_coord - on_axis).unsigned_abs();
            let b_dist = (b_coord - on_axis).unsigned_abs();
            a_dist
                .cmp(&b_dist)
                .then_with(|| a.dst_peer.cmp(&b.dst_peer))
                .then_with(|| a.dst_display.cmp(&b.dst_display))
        });
        let route = candidates[0];
        self.landing_for_route(route, cross)
    }

    /// High-speed crossing: previous sample inside source, current past edge.
    pub fn lookup_high_speed(
        &self,
        src_peer: &str,
        prev: (i32, i32),
        curr: (i32, i32),
    ) -> Option<Landing> {
        let dx = f64::from(curr.0 - prev.0);
        let dy = f64::from(curr.1 - prev.1);
        let dir = OutboundDir::from_delta(dx, dy)?;
        // Use the edge intersection inferred from the previous in-bounds point.
        let edge = dir.edge();
        let cross = match edge {
            DisplayEdge::Left | DisplayEdge::Right => prev.1,
            DisplayEdge::Top | DisplayEdge::Bottom => prev.0,
        };
        let edge_point = match edge {
            DisplayEdge::Left | DisplayEdge::Right => (curr.0, cross),
            DisplayEdge::Top | DisplayEdge::Bottom => (cross, curr.1),
        };
        self.lookup(src_peer, edge_point, dir)
            .or_else(|| self.lookup(src_peer, prev, dir))
    }

    fn landing_for_route(&self, route: &RouteInterval, src_cross: i32) -> Option<Landing> {
        let dst = self
            .displays
            .get(&(route.dst_peer.clone(), route.dst_display.clone()))?;
        // Map cross coordinate: intervals share workspace cross axis for axis-aligned adjacency.
        let dst_cross = src_cross.clamp(route.dst_start, route.dst_end.saturating_sub(1));
        let edge_coord = dst.workspace_rect.edge_coordinate(route.dst_edge);
        let workspace_landing = match route.dst_edge {
            DisplayEdge::Left => (edge_coord + LANDING_INSET, dst_cross),
            DisplayEdge::Right => (edge_coord - LANDING_INSET, dst_cross),
            DisplayEdge::Top => (dst_cross, edge_coord + LANDING_INSET),
            DisplayEdge::Bottom => (dst_cross, edge_coord - LANDING_INSET),
        };
        // Clamp into destination rect.
        let wx = workspace_landing
            .0
            .clamp(dst.workspace_rect.x(), dst.workspace_rect.right() - 1);
        let wy = workspace_landing
            .1
            .clamp(dst.workspace_rect.y(), dst.workspace_rect.bottom() - 1);
        let local = (wx - dst.local_origin.0, wy - dst.local_origin.1);
        Some(Landing {
            dst_peer: route.dst_peer.clone(),
            dst_display: route.dst_display.clone(),
            local_position: local,
            workspace_revision: self.workspace_revision,
            dst_topology_generation: dst.topology_generation,
        })
    }

    /// Converts a local device point into workspace coordinates.
    pub fn local_to_workspace(
        &self,
        peer: &str,
        display: &DisplayId,
        local: (i32, i32),
    ) -> Option<(i32, i32)> {
        let placed = self.displays.get(&(peer.to_string(), display.clone()))?;
        Some((
            local.0 + placed.local_origin.0,
            local.1 + placed.local_origin.1,
        ))
    }

    /// Finds which local display contains a local-space point.
    pub fn display_at_local(&self, peer: &str, local: (i32, i32)) -> Option<&DisplayId> {
        for ((p, id), placed) in &self.displays {
            if p != peer {
                continue;
            }
            let local_rect = placed
                .workspace_rect
                .translated(-placed.local_origin.0, -placed.local_origin.1)?;
            if local_rect.contains(local) {
                return Some(id);
            }
        }
        None
    }

    /// Returns placed workspace rect for a display, if present in this snapshot.
    pub fn workspace_rect(&self, peer: &str, display: &DisplayId) -> Option<DisplayRect> {
        self.displays
            .get(&(peer.to_string(), display.clone()))
            .map(|d| d.workspace_rect)
    }
}

/// Snap a free device placement so compatible edges become adjacent.
///
/// Moves `moving` by the smallest translation that makes any of its displays
/// share a full or partial edge with any display of `anchor`. Returns the new
/// origin for `moving`, or the original origin when no snap is within
/// `threshold` pixels.
pub fn snap_device_origin(
    moving_origin: (i32, i32),
    moving_topology: &LocalTopology,
    anchor_origin: (i32, i32),
    anchor_topology: &LocalTopology,
    threshold: i32,
) -> (i32, i32) {
    let threshold = threshold.max(0);
    let mut best: Option<(u32, (i32, i32))> = None;

    for m in &moving_topology.displays {
        for a in &anchor_topology.displays {
            let Some(a_rect) = a.rect.translated(anchor_origin.0, anchor_origin.1) else {
                continue;
            };
            let local_m = m.rect;
            // Keep the free axis; snap the adjacent axis so edges touch.
            let proper = [
                (a_rect.x() - local_m.right(), moving_origin.1),
                (a_rect.right() - local_m.x(), moving_origin.1),
                (moving_origin.0, a_rect.y() - local_m.bottom()),
                (moving_origin.0, a_rect.bottom() - local_m.y()),
            ];
            for &(ox, oy) in &proper {
                let dx = (ox - moving_origin.0).unsigned_abs();
                let dy = (oy - moving_origin.1).unsigned_abs();
                let dist = dx.saturating_add(dy);
                if dist as i32 > threshold {
                    continue;
                }
                let Some(snapped) = local_m.translated(ox, oy) else {
                    continue;
                };
                if !edges_would_overlap(snapped, a_rect) {
                    continue;
                }
                match best {
                    Some((best_dist, _)) if best_dist <= dist => {}
                    _ => best = Some((dist, (ox, oy))),
                }
            }
        }
    }
    best.map(|(_, origin)| origin).unwrap_or(moving_origin)
}

fn edges_would_overlap(a: DisplayRect, b: DisplayRect) -> bool {
    // Share a vertical edge with y overlap, or horizontal edge with x overlap.
    let vertical =
        (a.right() == b.x() || b.right() == a.x()) && a.y() < b.bottom() && b.y() < a.bottom();
    let horizontal =
        (a.bottom() == b.y() || b.bottom() == a.y()) && a.x() < b.right() && b.x() < a.right();
    vertical || horizontal
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> DisplayId {
        DisplayId::new(s).unwrap()
    }

    fn rect(x: i32, y: i32, w: u32, h: u32) -> DisplayRect {
        DisplayRect::new(x, y, w, h).unwrap()
    }

    fn display(name: &str, x: i32, y: i32, w: u32, h: u32) -> LocalDisplay {
        LocalDisplay {
            id: id(name),
            rect: rect(x, y, w, h),
            scale: None,
            rotation: None,
        }
    }

    fn display_scaled(name: &str, x: i32, y: i32, w: u32, h: u32, scale: f32) -> LocalDisplay {
        LocalDisplay {
            id: id(name),
            rect: rect(x, y, w, h),
            scale: Some(scale),
            rotation: None,
        }
    }

    fn topo(displays: Vec<LocalDisplay>) -> LocalTopology {
        LocalTopology::try_new(displays, 1, 1).unwrap()
    }

    fn workspace(placements: Vec<(&str, i32, i32)>) -> WorkspaceConfig {
        WorkspaceConfig {
            workspace_id: "ws-test".into(),
            revision: 1,
            authority_fingerprint: "authority".into(),
            placements: placements
                .into_iter()
                .map(|(fp, x, y)| DevicePlacement {
                    peer_fingerprint: fp.into(),
                    origin_in_workspace: (x, y),
                })
                .collect(),
        }
    }

    fn topologies(pairs: Vec<(&str, LocalTopology)>) -> BTreeMap<PeerFingerprint, LocalTopology> {
        pairs
            .into_iter()
            .map(|(fp, t)| (fp.to_string(), t))
            .collect()
    }

    /// 1. One monitor each, adjacent horizontally and vertically.
    #[test]
    fn two_devices_adjacent_horizontal_and_vertical() {
        let a = topo(vec![display("a1", 0, 0, 1920, 1080)]);
        let b = topo(vec![display("b1", 0, 0, 1920, 1080)]);
        let ws = workspace(vec![("A", 0, 0), ("B", 1920, 0)]);
        let snap =
            derive_routes(&ws, &topologies(vec![("A", a.clone()), ("B", b.clone())])).unwrap();
        let landing = snap
            .lookup("A", (1919, 540), OutboundDir::Right)
            .expect("horizontal crossing");
        assert_eq!(landing.dst_peer, "B");
        assert_eq!(landing.dst_display, id("b1"));
        assert_eq!(landing.local_position.0, LANDING_INSET);
        assert_eq!(landing.local_position.1, 540);

        let ws_v = workspace(vec![("A", 0, 0), ("B", 0, 1080)]);
        let snap_v = derive_routes(&ws_v, &topologies(vec![("A", a), ("B", b)])).unwrap();
        let landing_v = snap_v
            .lookup("A", (960, 1079), OutboundDir::Down)
            .expect("vertical crossing");
        assert_eq!(landing_v.dst_peer, "B");
        assert_eq!(landing_v.local_position.1, LANDING_INSET);
    }

    /// 2. Laptop adjacent to only the middle portion of a larger monitor.
    #[test]
    fn partial_edge_adjacency() {
        let desk = topo(vec![display("desk", 0, 0, 3840, 2160)]);
        let laptop = topo(vec![display("lap", 0, 0, 1440, 900)]);
        // Place laptop against the middle of the desk's right edge.
        let lap_y = (2160 - 900) / 2;
        let ws = workspace(vec![("DESK", 0, 0), ("LAP", 3840, lap_y)]);
        let snap = derive_routes(&ws, &topologies(vec![("DESK", desk), ("LAP", laptop)])).unwrap();

        // Middle of shared band crosses.
        let mid = lap_y + 450;
        let hit = snap
            .lookup("DESK", (3839, mid), OutboundDir::Right)
            .expect("partial mid");
        assert_eq!(hit.dst_peer, "LAP");

        // Above the laptop band is a wall.
        assert!(
            snap.lookup("DESK", (3839, 10), OutboundDir::Right)
                .is_none()
        );
        assert!(
            snap.lookup("DESK", (3839, 2150), OutboundDir::Right)
                .is_none()
        );
    }

    /// 3. Three staggered Windows monitors + MacBook.
    #[test]
    fn staggered_windows_plus_macbook() {
        // Windows: left 1080x1920 portrait at (-1080,0), center 1920x1080, right 1920x1080 at (1920,200)
        let win = topo(vec![
            display("w-left", -1080, 0, 1080, 1920),
            display("w-center", 0, 0, 1920, 1080),
            display("w-right", 1920, 200, 1920, 1080),
        ]);
        let mac = topo(vec![display("mac", 0, 0, 1512, 982)]);
        // Mac sits to the right of w-right, aligned to part of that monitor.
        let ws = workspace(vec![("WIN", 0, 0), ("MAC", 1920 + 1920, 200)]);
        let snap = derive_routes(&ws, &topologies(vec![("WIN", win), ("MAC", mac)])).unwrap();
        let y = 200 + 400;
        let landing = snap
            .lookup("WIN", (1920 + 1920 - 1, y), OutboundDir::Right)
            .expect("mac adjacent to right monitor");
        assert_eq!(landing.dst_peer, "MAC");
        // Crossing from left portrait's outer left is a wall (no peer).
        assert!(
            snap.lookup("WIN", (-1080, 500), OutboundDir::Left)
                .is_none()
        );
    }

    /// 4. Negative origins and portrait monitors.
    #[test]
    fn negative_origins_and_portrait() {
        let a = topo(vec![display("portrait", -1200, -100, 1200, 1920)]);
        let b = topo(vec![display("main", 0, 0, 1920, 1080)]);
        let ws = workspace(vec![("A", 0, 0), ("B", 0, 0)]);
        // A local already has negative origin; placement at 0 keeps it.
        // B at 0 overlaps A's positive area? A is -1200..0, B is 0..1920 — touch at x=0, no area overlap.
        let snap = derive_routes(&ws, &topologies(vec![("A", a), ("B", b)])).unwrap();
        let landing = snap
            .lookup("A", (-1, 500), OutboundDir::Right)
            .expect("cross from portrait to main");
        assert_eq!(landing.dst_peer, "B");
    }

    /// 5. Internal same-device monitor boundaries remain non-routes.
    #[test]
    fn internal_same_device_boundaries_not_routed() {
        let dual = topo(vec![
            display("left", 0, 0, 1920, 1080),
            display("right", 1920, 0, 1920, 1080),
        ]);
        let other = topo(vec![display("peer", 0, 0, 800, 600)]);
        let ws = workspace(vec![("HOST", 0, 0), ("PEER", 4000, 0)]);
        let snap = derive_routes(
            &ws,
            &topologies(vec![("HOST", dual.clone()), ("PEER", other)]),
        )
        .unwrap();
        // Internal seam at x=1920 must not create a host→host route.
        assert!(snap.routes().iter().all(|r| r.src_peer != r.dst_peer));
        let segments = boundary_segments(&dual);
        // Internal right of left / left of right should be omitted.
        assert!(!segments.iter().any(|s| {
            s.display_id == id("left") && s.edge == DisplayEdge::Right && s.start < 1080
        }));
    }

    /// 6. Multiple peers on separate portions of the same edge.
    #[test]
    fn multiple_peers_on_disjoint_edge_intervals() {
        let host = topo(vec![display("host", 0, 0, 3000, 1000)]);
        let top_peer = topo(vec![display("t", 0, 0, 1000, 800)]);
        let bot_peer = topo(vec![display("b", 0, 0, 1000, 800)]);
        let ws = workspace(vec![("HOST", 0, 0), ("TOP", 3000, 0), ("BOT", 3000, 200)]);
        // TOP occupies y=0..800 on the right; BOT at y=200 overlaps TOP in workspace — invalid.
        assert!(matches!(
            derive_routes(
                &ws,
                &topologies(vec![
                    ("HOST", host.clone()),
                    ("TOP", top_peer.clone()),
                    ("BOT", bot_peer.clone()),
                ])
            ),
            Err(WorkspaceError::OverlappingDevices(_, _))
        ));

        // Non-overlapping: TOP at y=0..800, BOT at y=800..1600.
        let ws2 = workspace(vec![("HOST", 0, 0), ("TOP", 3000, 0), ("BOT", 3000, 800)]);
        let snap = derive_routes(
            &ws2,
            &topologies(vec![("HOST", host), ("TOP", top_peer), ("BOT", bot_peer)]),
        )
        .unwrap();
        assert_eq!(
            snap.lookup("HOST", (2999, 100), OutboundDir::Right)
                .unwrap()
                .dst_peer,
            "TOP"
        );
        assert_eq!(
            snap.lookup("HOST", (2999, 900), OutboundDir::Right)
                .unwrap()
                .dst_peer,
            "BOT"
        );
    }

    /// 7. Inward-facing adjacency in a U-shaped arrangement.
    #[test]
    fn inward_facing_u_shape_adjacency() {
        // Host U: left column, top bar, right column — gap in the middle bottom.
        let host = topo(vec![
            display("left", 0, 0, 400, 1200),
            display("top", 400, 0, 800, 400),
            display("right", 1200, 0, 400, 1200),
        ]);
        // Guest sits inside the U, against the inward-facing right edge of left column
        // and left edge of right column? Place against bottom of top bar (inward).
        let guest = topo(vec![display("guest", 0, 0, 800, 400)]);
        let segments = boundary_segments(&host);
        assert!(
            segments.iter().any(|s| {
                s.display_id == id("top") && s.edge == DisplayEdge::Bottom && s.start >= 400
            }),
            "top display bottom facing the U void must be a boundary segment: {segments:?}"
        );

        let ws = workspace(vec![("HOST", 0, 0), ("GUEST", 400, 400)]);
        let snap = derive_routes(&ws, &topologies(vec![("HOST", host), ("GUEST", guest)])).unwrap();
        let landing = snap
            .lookup("HOST", (800, 399), OutboundDir::Down)
            .expect("inward bottom of top bar");
        assert_eq!(landing.dst_peer, "GUEST");
        // Round-trip back up.
        let back = snap
            .lookup("GUEST", (400, 0), OutboundDir::Up)
            .expect("return crossing");
        assert_eq!(back.dst_peer, "HOST");
        assert_eq!(back.dst_display, id("top"));
    }

    /// 8. Gaps, ambiguous overlaps, corners, and interval endpoints.
    #[test]
    fn gaps_corners_and_endpoints() {
        let a = topo(vec![display("a", 0, 0, 1000, 1000)]);
        let b = topo(vec![display("b", 0, 0, 1000, 1000)]);
        // Gap of 10px — no route.
        let ws_gap = workspace(vec![("A", 0, 0), ("B", 1010, 0)]);
        let snap_gap = derive_routes(
            &ws_gap,
            &topologies(vec![("A", a.clone()), ("B", b.clone())]),
        )
        .unwrap();
        assert!(
            snap_gap
                .lookup("A", (999, 500), OutboundDir::Right)
                .is_none()
        );

        // Overlap → error.
        let ws_overlap = workspace(vec![("A", 0, 0), ("B", 500, 0)]);
        assert!(matches!(
            derive_routes(
                &ws_overlap,
                &topologies(vec![("A", a.clone()), ("B", b.clone())])
            ),
            Err(WorkspaceError::OverlappingDevices(_, _))
        ));

        let ws = workspace(vec![("A", 0, 0), ("B", 1000, 0)]);
        let snap = derive_routes(&ws, &topologies(vec![("A", a), ("B", b)])).unwrap();
        // Half-open: y=0 included, y=1000 excluded.
        assert!(snap.lookup("A", (999, 0), OutboundDir::Right).is_some());
        assert!(snap.lookup("A", (999, 999), OutboundDir::Right).is_some());
        assert!(snap.lookup("A", (999, 1000), OutboundDir::Right).is_none());
    }

    /// 9. Round-trip crossing-position consistency.
    #[test]
    fn round_trip_crossing_consistency() {
        let a = topo(vec![display("a", 0, 0, 1920, 1080)]);
        let b = topo(vec![display("b", 0, 0, 1920, 1080)]);
        let ws = workspace(vec![("A", 0, 0), ("B", 1920, 0)]);
        let snap = derive_routes(&ws, &topologies(vec![("A", a), ("B", b)])).unwrap();
        for y in [0, 100, 540, 1079] {
            let to_b = snap.lookup("A", (1919, y), OutboundDir::Right).unwrap();
            assert_eq!(to_b.local_position.1, y);
            let back = snap
                .lookup("B", (to_b.local_position.0, y), OutboundDir::Left)
                .unwrap();
            assert_eq!(back.dst_peer, "A");
            assert_eq!(back.local_position.1, y);
            // Landed inset from right edge of A.
            assert_eq!(back.local_position.0, 1919 - LANDING_INSET);
        }
    }

    /// 10. Mixed-scale coordinate transforms (logical space; scales informational).
    #[test]
    fn mixed_scale_logical_transforms() {
        let retina = topo(vec![display_scaled("retina", 0, 0, 1512, 982, 2.0)]);
        let desk = topo(vec![display_scaled("desk", 0, 0, 1920, 1080, 1.0)]);
        let ws = workspace(vec![("MAC", 0, 0), ("PC", 1512, 0)]);
        let snap = derive_routes(&ws, &topologies(vec![("MAC", retina), ("PC", desk)])).unwrap();
        // Routing uses logical rects, not physical pixels.
        let landing = snap
            .lookup("MAC", (1511, 400), OutboundDir::Right)
            .expect("logical adjacency");
        assert_eq!(landing.dst_peer, "PC");
        assert_eq!(landing.local_position, (LANDING_INSET, 400));
    }

    /// 11. High-speed crossings.
    #[test]
    fn high_speed_crossing() {
        let a = topo(vec![display("a", 0, 0, 1000, 1000)]);
        let b = topo(vec![display("b", 0, 0, 1000, 1000)]);
        let ws = workspace(vec![("A", 0, 0), ("B", 1000, 0)]);
        let snap = derive_routes(&ws, &topologies(vec![("A", a), ("B", b)])).unwrap();
        // Previous inside, current jumped past the edge.
        let landing = snap
            .lookup_high_speed("A", (990, 400), (1020, 405))
            .expect("high speed");
        assert_eq!(landing.dst_peer, "B");
    }

    /// 12. Display removal and topology-generation changes.
    #[test]
    fn display_removal_and_generation() {
        let a1 = topo(vec![
            display("a1", 0, 0, 1920, 1080),
            display("a2", 1920, 0, 1920, 1080),
        ]);
        let b = topo(vec![display("b1", 0, 0, 800, 600)]);
        let ws = workspace(vec![("A", 0, 0), ("B", 3840, 0)]);
        let snap1 = derive_routes(&ws, &topologies(vec![("A", a1), ("B", b.clone())])).unwrap();
        assert!(snap1.lookup("A", (3839, 100), OutboundDir::Right).is_some());

        // Remove a2; B no longer adjacent.
        let a2 = LocalTopology::try_new(vec![display("a1", 0, 0, 1920, 1080)], 1, 2).unwrap();
        let snap2 = derive_routes(&ws, &topologies(vec![("A", a2), ("B", b)])).unwrap();
        assert!(snap2.lookup("A", (1919, 100), OutboundDir::Right).is_none());
        assert_eq!(snap2.workspace_revision, 1);
    }

    #[test]
    fn unauthorized_overlap_reports_both_peers() {
        let a = topo(vec![display("a", 0, 0, 100, 100)]);
        let b = topo(vec![display("b", 0, 0, 100, 100)]);
        let ws = workspace(vec![("A", 0, 0), ("B", 50, 50)]);
        match derive_routes(&ws, &topologies(vec![("A", a), ("B", b)])) {
            Err(WorkspaceError::OverlappingDevices(x, y)) => {
                assert!(x == "A" || y == "A");
                assert!(x == "B" || y == "B");
            }
            other => panic!("expected overlap, got {other:?}"),
        }
    }

    #[test]
    fn offline_peer_placement_preserved_without_routes() {
        let a = topo(vec![display("a", 0, 0, 100, 100)]);
        let ws = workspace(vec![("A", 0, 0), ("OFFLINE", 200, 0)]);
        let snap = derive_routes(&ws, &topologies(vec![("A", a)])).unwrap();
        assert!(snap.routes().is_empty());
        assert_eq!(snap.workspace_revision, 1);
    }

    #[test]
    fn snap_aligns_compatible_edges() {
        let a = topo(vec![display("a", 0, 0, 100, 100)]);
        let b = topo(vec![display("b", 0, 0, 100, 100)]);
        let snapped = snap_device_origin((108, 5), &b, (0, 0), &a, 20);
        assert_eq!(snapped, (100, 5));
    }
}
