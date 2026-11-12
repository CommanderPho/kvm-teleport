# Shared Spatial Layout

Mousehop can place individual monitors from multiple LAN peers into one
shared workspace. Cursor crossings follow **actual adjacent edge intervals**,
not whole-desktop left/right/top/bottom boxes.

## Concepts

| Layer | Meaning |
|-------|---------|
| Device identity | TLS certificate fingerprint (authenticated). Display names are labels only. |
| Local topology | Per-monitor rectangles in each OS’s capture/warp coordinate system, with stable-ish display ids. |
| Workspace | Versioned placements of each device’s local origin; monitors keep their OS-relative arrangement. |
| Routing snapshot | Immutable table derived from workspace + live topologies; hot path only looks up against it. |

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
