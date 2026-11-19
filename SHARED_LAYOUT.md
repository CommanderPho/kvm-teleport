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

- Legacy: `position = "left|right|top|bottom"` in `config.toml` (unchanged; silent
  default `left` for barrier create / migration when needed).
- Spatial: `workspace.toml` next to `config.toml` (absent ⇒ legacy-only).

Missing `workspace.toml` does not break existing installs.

**Legacy → spatial conversion:** Opening the shared-layout editor drafts /
seeds placements for this host plus every **outgoing client that already has a
TLS fingerprint** (known after first successful connect). Origins are taken from
each client’s legacy `position` relative to the local monitor AABB. Clients that
have never connected stay off the canvas until their fingerprint is known; the
arranger then picks them up live.

Device labels prefer OS / DNS hostnames (`MacBookPro.lan`) over fingerprint
prefixes.

## GUI planner

### First-time setup (Setup Layout)

On each outgoing client row, **Setup Layout**:

1. Requires a hostname.
2. Connects the peer (`Activate`) and waits for its TLS fingerprint.
3. Fetches the workspace and opens the Shared layout editor with that peer
   selected for positioning.
4. After you **Apply**, the peer is in `workspace.placements` and the Enable
   switch may turn capture on for that peer.

Until a peer is placed (fingerprint present in applied placements), enabling the
outgoing client is blocked with a toast pointing at Setup Layout.

### Editing an applied workspace (Shared layout…)

**Outgoing Connections → Shared layout…** (and About → build row) is **edit-only**.
It is sensitive only when a workspace has already been applied
(`workspace_id` non-empty and `revision > 0`). First-time placement uses
**Setup Layout** on the client row, not this button.

In the editor:

- Peer list shows each host with nested, read-only displays.
- Canvas draws every host’s full monitor set in workspace space.
- Authority drags **host groups**, optional edge snap, Fit/zoom, X/Y spins.
- Route interval highlights and overlap/validation errors on the draft.
- Claim / Apply / Revert / Close. Followers are view-only.

The outgoing Position combo has been removed from the GUI. Legacy `position` in
config remains for IPC compatibility and barrier seeding; spatial placement is
owned by the workspace after Apply.

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
