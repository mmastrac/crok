# procstream plan

Deferred features, roughly in priority order.

## Job::pids() tree enumeration

`Job::pids() -> io::Result<Vec<u32>>` listing every process currently in the
tree.

- Windows: `QueryInformationJobObject(JobObjectBasicProcessIdList)`. Exact and
  race-free, since the Job object tracks membership through orphaning. Requires
  keeping the job handle in `signal` (query needs it after a terminate) rather
  than `take()`-ing it.
- macOS: `proc_listpids(PROC_PGRP_ONLY, pgid)` is a single call. Add the
  wrapper to the `proc_pidinfo` crate rather than binding libproc here.
  `ProcBSDShortInfo.pbsi_status` distinguishes zombies.
- Linux: scan `/proc/*/stat` and match field 5 (pgrp). Parse from the last `)`
  so a comm with spaces does not break the field split. State field `Z` marks
  zombies.

Unlocks:

- A zombie-aware liveness probe for `Job::shutdown` in place of
  `kill(-pgid, 0)`, which cannot tell a zombie from a live process.
- Diagnostics: report what survived the grace period, by pid and name.
- A crok feature: report processes a test leaked.

## Kill-on-drop and detach()

Dropping a `Child` today kills the tree on Windows (kill-on-close fires when
the Arc drops the job handle) but leaks it on Unix. Make kill-on-drop the
default on both platforms, with an explicit `detach()` for callers that want
the tree to outlive the handle. Decide whether the `Job` clones keep the tree
alive (likely yes: kill when the last handle drops).

## Readiness reactor

A thread-free, runtime-free capture backend built on `rustix`, slotting in
behind `Output` without an API change. On Linux, `pidfd` also makes the exit a
readiness event, replacing the watcher thread. Design exists, not started.

## Smaller items

- `Overwrite::CollapseBlock`: resolve rewrites that span lines (cursor-up plus
  erase). Currently falls back to `CollapseLine`.
- `Output::recv_deadline(Instant)`: saves the subtract-and-clamp dance every
  deadline-driven consumer writes around `recv_timeout`.
- `#[cfg(feature = "async")] Output::poll_next`: awaitable chunks backed by the
  same channel, no runtime dependency.
