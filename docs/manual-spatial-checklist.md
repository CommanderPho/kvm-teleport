# Manual spatial-layout checklist (Windows ↔ macOS)

Automated unit tests cover geometry, protocol codecs, and workspace
persistence. The following require real hardware / OS permissions.

## Permissions

- [ ] macOS: Accessibility + Input Monitoring granted; Screen Recording if prompted
- [ ] Windows: run elevated only if your environment requires it for low-level hooks
- [ ] Both peers show as authorized fingerprints (no pairing secret in workspace data)

## Layout & routing

- [ ] Authority opens **About → build row → shared layout**, places two device groups so edges touch on a partial interval, Apply
- [ ] Follower receives same revision (check `workspace.toml` / GUI revision)
- [ ] Cursor crosses only on the highlighted overlap; unmatched edge is a wall
- [ ] Cross back lands inset on the correct monitor
- [ ] Third peer placed without a single global offset; routes to each independently

## Failure / recovery

- [ ] Emergency release chord returns local control
- [ ] Sleep/wake of peer releases remote held keys and restores local input
- [ ] Unplug destination display during remote control; ownership recovers
- [ ] Reconnect with cached layout; hotplug does not attach old id by enum order
- [ ] Change layout while input is remote; next crossing uses new revision

## Input quality

- [ ] High polling-rate mouse: no bounce-back, no noticeable handoff delay
- [ ] Held modifiers/buttons survive handoff and clear on disconnect
- [ ] Mixed DPI: logical alignment acceptable; do not expect physical mm match

## Legacy

- [ ] Peer without shared-workspace capability still works via left/right/top/bottom
