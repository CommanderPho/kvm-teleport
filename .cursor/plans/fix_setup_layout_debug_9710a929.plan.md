---
name: Fix Setup Layout debug
overview: Add immediate Setup Layout progress feedback so the first click is not a silent wait, and switch Windows debugging to cppvsdbg because CodeLLDB is aborting with an access violation before/during the session.
todos:
  - id: fix-launch-cppvsdbg
    content: Make Windows MSVC cppvsdbg the only/primary debug launch; drop broken CodeLLDB entry
    status: completed
  - id: setup-layout-feedback
    content: Toast Connecting… on Setup Layout; avoid silent wait; optional still-connecting on re-click
    status: completed
  - id: verify-debug-setup
    content: Sanity-check launch config + note manual F5 / Setup Layout check
    status: completed
isProject: false
---

# Fix Setup Layout hang feedback + Windows debug crash

## What happened

**Setup Layout (first click)** — flow in [`window.rs`](mousehop-gtk/src/window.rs) when the peer has no fingerprint yet:

1. `Activate(true)`
2. Wait for `peer_fingerprint` (up to 25s)
3. `GetWorkspace` → open editor

There is **no toast/spinner until timeout**. A slow first connect looks like “nothing happened.” The second click usually already has a fingerprint from the first Activate, so it opens immediately — matching what you saw.

**LLDB crash** — log exit code `3221225477` = `0xC0000005` (ACCESS_VIOLATION). Adapter fields `liblldb` / `lldbServer` are **empty**, then the adapter dies right after “Launching adapter.” That is CodeLLDB failing on **MSVC** Rust (`x86_64-pc-windows-msvc`), not a proven Mousehop panic. Use **C/C++ `cppvsdbg`** for F5 on this machine.

## Plan

### 1. Debugger configs — [`kvm-teleport.code-workspace`](kvm-teleport.code-workspace)

- Keep **Debug mousehop (Windows MSVC)** (`cppvsdbg`) as the primary Windows config.
- Remove the **CodeLLDB** launch entry on this Windows-focused workspace (or rename/disable it with a comment that it is unsupported for MSVC here).
- Ensure MSVC launch `env.Path` includes GTK bin (already does).
- Confirm **ms-vscode.cpptools** is installed; F5 that config, not CodeLLDB.

### 2. Setup Layout progress UX — [`mousehop-gtk/src/window.rs`](mousehop-gtk/src/window.rs)

In `begin_setup_layout`:

- If hostname missing → toast (unchanged).
- If fingerprint **missing**: toast immediately, e.g. `Connecting to {hostname}…` before `Activate`.
- If fingerprint **present**: toast `Opening shared layout…` (optional, brief).
- On timeout: keep existing failure toast.
- On successful open (`finish_setup_layout_open`): no extra toast required.

Optional small polish: if Setup Layout is clicked again while `pending_setup_handle` is already that handle, show `Still connecting…` instead of restarting Activate/generation (avoids cancelling an in-flight wait). Prefer this if easy; otherwise progress toast alone is enough.

### 3. Verify

- F5 with **Debug mousehop (Windows MSVC)**; open prefs; click Setup Layout once on `MacBookPro.lan` → see Connecting toast → editor opens without needing a second click (when peer is reachable).
- Confirm CodeLLDB is no longer the selected config.

## Out of scope

- Replacing Position/capture barriers
- Fixing CodeLLDB upstream for MSVC
- Deep crash forensics unless cppvsdbg still dies when the editor opens (then investigate `workspace_editor` separately)
