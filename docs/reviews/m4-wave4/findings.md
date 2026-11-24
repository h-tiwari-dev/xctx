# Milestone 4 Wave 4: review findings

Three read-only reviewers ran on `git diff 9ff2719..471fdca`. They used the prompt in
[`../m2-wave4/reviewer-prompt.md`](../m2-wave4/reviewer-prompt.md), one reviewer per area, and read the code only:
nothing was built or run. Line numbers refer to that diff.

Work started after the review already fixed two things: a new space's `PORT` now avoids attempt ports
(`Store::allocate_port_avoiding`), and adopt stops the attempt's agent before taking the snapshot.

## Core: `xctx-core` attempts and git, and `xctx-attempt`

**Critical**
- C1. Discarding an attempt removes the shared `/mise.<slug>.toml` line from `info/exclude`
  (`attempt.rs` `discard_attempt` → `mise::remove_space_mise_file` → `remove_git_exclude`). After that:
  - the space's own mise file shows up as untracked;
  - every later adopt fails with `ChangedSinceFork`;
  - other attempts' diffs include their mise file.

  Fix: delete only the attempt's file and keep the exclude line, which the space still owns. Test: fork 2, discard 1,
  `git status` of the space is clean, then adopt 2.
- C2. `snapshot_commit` starts its temp index with `read-tree HEAD`, which loses the sparse-checkout, skip-worktree
  and assume-unchanged bits:
  - in a sparse checkout, files outside the cone are snapshotted (and adopted) as deleted;
  - skip-worktree files get their local contents committed.

  Also, `stash push` can save nothing while xctx still believes it stashed, so `stash drop` or `stash pop` then acts
  on an unrelated stash of the user's. Fix: copy the real index (`git rev-parse --git-path index`) into the temp
  index; only pop or drop a stash xctx made (compare `refs/stash` before and after), or keep the backup as a ref
  instead of a stash.

**Warnings**
- W1. Work written in the space between the `ChangedSinceFork` check and `stash push` ends up in the stash and is
  then dropped. Re-check right before dropping, or keep the stash when it differs from the fork.
- W2. `git merge` follows the user's `merge.ff` setting. Pass `--ff` explicitly, and work out `fast_forward` from the
  result.
- W3. A failed fork's rollback swallows its errors, and attempt numbers are only used up in `record_fork`. Leftover
  branches or folders then make every later fork fail with `AttemptBranchTaken`. Reserve numbers persistently, or
  skip taken ones, and report rollback failures.
- W4. `git diff` follows user config (`diff.external`, `color.diff`) and reads `path` as a pathspec. Add
  `--no-ext-diff --no-color` and literal pathspecs.

**Suggestions**
- In the one-file diff, follow renames.
- `fork_args` by id can fall back to a prefix match. Pass the session file path when it is known, and consider a
  separator between the parts that slugs cannot contain.
- `store::load` should check that `space` matches and that `next_number` is above every stored attempt number.
- The per-round adoption limit and the docs disagree; align them.
- Add tests: discard then adopt, a user stash that is left alone, and a sparse checkout.

## App: `crates/xctx`, plus the control and Inbox parts

**Warnings**
- A1. The legacy session lookup (`agent_session.rs` `newest_session_in`) can pick another agent's session: helpers
  share the writer's worktree. Skip `xctx-*` named sessions, and refuse a fork whose writer has no recorded session.
- A2. A session id is recorded on the graph node before pi has started. When pi is missing or fails to spawn, the old
  agent's session is orphaned. Record the id only after a successful spawn of `pi` itself.
- A3. A fork can use a source session that is not on disk yet: a fresh lane, or the adopted id after
  `continue_writer_from` returned early. Check `has_session`, or use the winning attempt's session.
- A4. The writer's agent keeps running during an adopt merge, so some of its edits are lost. Stop it, and wait for it
  to exit, before `adopt_attempt`; start it again if the adopt is refused.
- A5. `attempt.report` does not check the caller, so any pane can overwrite another attempt's outcome. Give each
  attempt a token (`XCTX_ATTEMPT_TOKEN`, stored in `attempts.json`) and require it.
- A6. Ports: already fixed after the review (see above).
- A7. If marking an attempt adopted cannot be saved, the cleanup still runs, leaving the records inconsistent. Stop
  and show the error instead.
- A8. Cleanup after a failed fork record is not tracked as an attempt job (quitting is not blocked) and its errors
  are dropped. Track it and report failures.

**Suggestions**
- Do not start `pi` again while an old process for the same session may still be exiting.
- Attempt panes export `XCTX_AGENT_ID=<writer>`. Use a distinct id, so an attempt cannot act as the writer in
  contracts and the loop.
- Show the error when `attempts.json` cannot be read, instead of quietly opening an empty session.

## UI: `xctx-ui`

**Warnings**
- U1. `ForkDialog` offers more attempts than the space has room for, and with `max == 0` still enables Fork. Treat
  `max` as the remaining capacity, and disable Fork when it is 0.
- U2. A busy dialog still looks editable: the pills are not dimmed, and the app's inputs are not locked. Add
  `Pill::disabled`.
- U3. While a fork runs, Cancel only closes the dialog. Label it Close, or say that the fork carries on.
- U4. An Adopted chip can be selected, but its worktree and agent are gone. Only live attempts should be selectable.
- U5. The strip's Fork… and Review buttons have no busy state. Add `.busy(bool)`.
- U6. `ReviewView::busy: Option<u32>` does not match what the app knows (a job per space). Split it into
  `busy: bool` and `working: Option<u32>`.
- U7. Adopt is offered on Running attempts. Exclude them, or require a confirming click.
- U8. Discard is one click with no confirmation. Add a two-step confirm.
- U9. `files: None` always reads "Changes not computed yet", even when the worktree is gone. Base the text on
  whether the attempt is live.
- U10. The review sheet has no height limits: many files or a long summary push Adopt and Discard off-screen. Cap
  and scroll the file list and the summary.
- U11. Diff lines are not cut in length. Cut each line and cap the total.
- U12. `actions_are_disabled_while_one_runs` would pass even if the behaviour were broken. Add a positive control and
  assert that the error is shown.

**Suggestions**
- Document "already masked" on every text field of the view types.
- Give the active chip and the open file a cue besides colour.
- Work out `adopted` from the columns as well.
- Show the full instruction as a tooltip.
- Add tests for the diff line cap, the fallback instruction text, `max == 0` and the empty sheet's message.
