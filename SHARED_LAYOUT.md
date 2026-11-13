# Shared Spatial Layout

Mousehop can place each machine’s **rigid monitor set** into one shared
workspace. Cursor crossings follow **actual adjacent edge intervals**,
not whole-desktop left/right/top/bottom boxes.

## Concepts

| Layer | Meaning |
|-------|---------|
| Device identity | TLS certificate fingerprint (authenticated). Display names are labels only. |
| Local topology | Per-monitor rectangles in each OS’s capture/warp coordinate system, with stable-ish display ids. |
| Workspace | Versioned placements of each device’s local origin; monitors keep their OS-relative arrangement. |
| Routing snapshot | Immutable table derived from workspace + live topologies; hot path only looks up against it. |

**Placement unit:** one device-group origin `(origin_x, origin_y)` per machine
in continuous shared X/Y space (not a cardinal L/R/T/B grid). Hosts may sit at
any integer origin so partial edges can touch (for example a laptop under the
right half of a desk ultrawide). Per-monitor free placement that breaks the
host’s OS layout is out of scope.

**Workspace unit:** logical points as reported by platform APIs (macOS Quartz
points, Windows virtual-screen pixels). Physical millimetre calibration is out
of scope. Mixed-DPI peers align in logical space.

## Authority model

- One fingerprint is `authority_fingerprint` in `workspace.toml`.
- Only the authority’s publishes are accepted; followers validate, cache, and persist.
- No CRDT / automatic election. Change authority explicitly (GUI “Claim authority”).

## Protocol

Capability bit `CAP_SHARED_WORKSPACE` is negotiated in `Hello`.

| Message | Role |
|---------|------|
| `IdentifiedTopology` | Per-display ids + scale alongside legacy `DisplayLayout` |
| `WorkspaceChunk` / `WorkspaceAck` | Bounded DTLS snapshot sync with retry |
| `HandoverEnterSpatial` | Absolute per-display landing + workspace/topology revisions |

Peers without the capability keep legacy `Position` + `cross_fraction` handoffs.

## Configuration

- Legacy: `position = "left|right|top|bottom"` in `config.toml` (unchanged).
- Spatial: `workspace.toml` next to `config.toml` (absent ⇒ legacy-only).

Missing `workspace.toml` does not break existing installs.

## GUI planner

Open **Outgoing Connections → Shared layout…** (also About → build row).

- Peer list shows each host with nested, read-only displays.
- Canvas draws every host’s full monitor set in workspace space.
- Authority drags **host groups**, optional edge snap, Fit/zoom, X/Y spins.
- Route interval highlights and overlap/validation errors on the draft.
- Claim / Apply / Revert / Close. Followers are view-only.
- When a peer is placed in the workspace, that row’s legacy Position combo
  is disabled and labeled “managed by shared workspace layout”.

## Capture backends

- **Windows / macOS:** arm route segments from the active snapshot (including
  inward-facing edges when the OS pointer can leave that edge).
- **Linux (libei / layer-shell):** still use Position outer barriers; spatial
  geometry is computed but not armed on those backends yet.

Where the OS clips the pointer so it never leaves a void-facing edge, that
transition cannot fire at runtime even if geometry lists it.

## Hot path

Topology/layout changes rebuild `RoutingSnapshot`. Mouse events only perform
interval lookup against the current immutable snapshot — no disk I/O, no
network round-trips, no geometry rebuild per event.
