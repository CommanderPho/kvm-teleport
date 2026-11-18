---
name: Fix layout drag panic
overview: Fix the RefCell reentrancy panic triggered when dragging a device in the shared-layout editor (sync_spins emits value-changed while holding a borrow), and make the full drag/snap/apply rearranging flow panic-free.
todos:
  - id: sync-guard
    content: Add syncing Cell guard; sync_spins sets it around set_value; spin value-changed handlers early-return while set; read origins in scoped borrow
    status: completed
  - id: borrow-tightening
    content: refresh_ui snapshots strings before widget calls; draw func clones state instead of borrowing across draw_canvas
    status: completed
  - id: drag-polish
    content: drag_begin claims sequence on hit; drag_update refreshes preview routes live; keep drag_end snap/validate and Apply path
    status: completed
  - id: tests-lint
    content: Unit tests for selected_origin + guard predicate; cargo test/fmt/clippy on mousehop-gtk
    status: completed
  - id: verify-drag
    content: Rebuild Windows release; manual drag/snap/apply/revert/zoom check with no panic
    status: in_progress
isProject: false
---

# Fix layout-editor drag panic (RefCell reentrancy)

## Diagnosis (verified from `%LOCALAPPDATA%\Mousehop\logs\panic.log` + code)

- Panic: `BorrowMutError` in the `spin_x` `value-changed` closure, then `panic_cannot_unwind` at gtk4-rs `value_changed_trampoline` (extern "C") -> abort; debugger lands in `std/panicking.rs`.
- Chain: `drag_update` -> [`sync_spins`](mousehop-gtk/src/workspace_editor.rs) holds `state.borrow()` (line 650) while `spin_x.set_value()` (658) synchronously emits `value-changed` -> handler `state.borrow_mut()` (331) panics.
- Same reentrancy class: [`refresh_ui`](mousehop-gtk/src/workspace_editor.rs) holds a borrow across `meta.set_text`/`error_banner` calls (240-260); draw func keeps a temporary borrow across `draw_canvas` (190).

## Change — single file: [mousehop-gtk/src/workspace_editor.rs](mousehop-gtk/src/workspace_editor.rs)

1. **Sync guard**: add `let syncing = Rc::new(Cell::new(false));` (import `std::cell::Cell`). Wrap the body of `sync_spins` in `syncing.set(true)` / `set(false)` (new `syncing: &Cell<bool>` param; update its two call sites: `refresh_ui` and `drag_update`). At the top of both `spin_x`/`spin_y` `connect_value_changed` closures: `if syncing.get() { return; }` so programmatic updates never reenter state. User-typed spin edits still apply (flag false).
2. **Tighten borrows so no GTK call runs under a borrow**:
   - `sync_spins`: read `(origin_x, origin_y)` in a scoped borrow, drop, then `set_value` (guard makes reentry a no-op).
   - `refresh_ui`: snapshot `(meta_text, error)` in a scoped borrow; set widgets after drop; keep existing `rebuild_peer_list`/`sync_spins` calls after drop.
   - draw func: `let st = state.borrow().clone();` (EditorState derives Clone via WorkspaceIpc) then `draw_canvas(cr, w, h, &st)`; `fit_view` keeps its own short borrow.
3. **Drag flow polish (full rearranging behavior)**:
   - `drag_begin`: set `gesture.set_state(Claimed)` on hit (currently only Denied on miss) so LMB drag reliably owns the sequence.
   - `drag_update`: after `sync_spins`, call `refresh_preview_routes(&state)` (outside borrows) so route highlights and the validation banner track the drag live, matching spin edits and `drag_end`.
   - Keep existing `drag_end` snap (`apply_snap`) + `validate_draft` + `refresh_ui`, and Apply -> `FrontendRequest::ApplyWorkspace` (daemon already validates/publishes; no daemon changes).

## Tests

- Add `#[cfg(test)] mod tests` in `workspace_editor.rs` (no GTK init needed): extract the pure "read selected placement origin" step of `sync_spins` into `fn selected_origin(state: &Rc<RefCell<EditorState>>) -> Option<(i32, i32)>` and test it returns the selected device's origin and `None` when unselected; test the guard contract with a plain `Cell<bool>` (set -> spin-handler early-return predicate true). Widget-level reentry verified manually below.

## Verify

- `cargo test -p mousehop-gtk`, `cargo fmt`, `cargo clippy -p mousehop-gtk --all-targets` (GTK env as in scripts/build-windows-release.ps1).
- `scripts/build-windows-release.ps1`; launch staged exe; Setup Layout -> drag remote with LMB: no panic, spins track, routes update live, release snaps near edges, Apply persists (daemon log `apply_workspace`), Revert restores, zoom/Fit still work.

## Out of scope

- Daemon/workspace protocol changes; follower view-only behavior; new layout features beyond drag/snap/apply.