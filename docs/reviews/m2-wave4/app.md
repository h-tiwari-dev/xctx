<!-- Wave 4 review of Milestone 2 (diff 5bb14a6..9e7034e), by a read-only pi reviewer. Findings are unverified until fixed or dismissed in docs/m2.md. -->

## Critical (must fix)

None found in this area.

## Warnings (should fix)

1. **Control requests take keyboard focus away from an open dialog.** `crates/xctx/src/space_tab.rs:376-382` (`layout_changed`), `:488-492` (`focus_pane_in`), `:653-655` (`reshape_in`)
   - Keyboard pane shortcuts do nothing while a dialog is open (`workspace.rs:431-438`). Control requests skip that check: they only test `is_shown(id)` and then call `focus_active_pane`.
   - Example: the user is typing a pull request title in the Ship dialog (`dialogs.rs:159`). An agent sends `pane.split`, `pane.focus`, `pane.zoom` or `space.reshape` for that space. Focus moves to a terminal behind the dialog, so the rest of what the user types goes into a shell.
   - Even with no dialog open, `pane.split` and `pane.focus` move focus out of the pane the user is typing in.
   - **Fix:** in all three places, only refocus when `self.is_shown(id) && self.dialog.is_none()`. Consider changing the layout's focused pane without moving window focus when the request came over the socket. At minimum, never take focus from a dialog.

2. **An agent pane added with `pane.split kind=agent` resumes an agent session that is already running.** `space_tab.rs:412` passes `AgentStart::Continue`. `spawn.rs:294` turns that into `pi --continue`, which reopens the most recent session in that worktree. That session is almost always the one the space's existing agent pane is using, so two `pi` processes end up writing to one session.
   - Keyboard splits only ever make shells, so this is new in M2.
   - **Fix:** start the new agent with no session flag (a fresh session). Use `--continue` only when the space has no other agent pane in that repo.

3. **A control request can quietly open a closed space and start all of its agents.** `control.rs:402-421` (`in_open_space`)
   - `pane.focus`, `pane.zoom`, `pane.split`, `pane.close` and `space.reshape` aimed at a space whose tab is not open call `open_space`. That restores the space and starts every pane, with each agent running `pi --continue`.
   - So an agent in space A can wake up every agent in space B without the user knowing. `layout.get` already shows a closed space's saved layout without opening it.
   - **Fix:** return `RpcError::conflict("space X is not open; open it first")` for a closed space. If opening is wanted, add an explicit `space.open` method that posts an Inbox notice. Either way, mention it in `docs/m2.md`.

4. **The Parent Agent's "writes no code" rule is only a prompt.** `crates/xctx/src/parent_agent.rs:1-4,20`
   - `--tools read,bash` gives it a full shell, so it can `sed -i`, `git commit`, or `rm`. It also runs inside the data directory (`parent-agent/` next to the store and the Inbox files), so it can edit the store or the Inbox file directly. That bypasses the Plan Gate the next time xctx starts.
   - The pi docs (`docs/cli.md:134`) also say `--tools` does not remove MCP tools, and the user's own pi extensions still load.
   - The module doc and `PARENT_AGENT_ROLE` (`xctx-ui/src/parent_agent.rs:52`) state this as a guarantee. `docs/m2.md` itself only promises a "read-only / no-code system prompt", so the code matches the spec but overstates it.
   - **Fix (best):** replace `bash` with a dedicated `xctx-ctl` tool, or a pi extension that allows only `xctx-ctl` and read-only commands.
   - **Fix (minimum):** change the doc comments and the UI text to say the restriction is advisory, and run the agent in a directory outside the data directory.

5. **"The value is masked from agents from now on" is only true until xctx restarts.** `inbox_view.rs:301` (`redactor.extend`), `:383`
   - The redactor lives in memory. After a restart, `check_mise` refills it from `mise env --json --redacted` (`xctx-core/src/mise_env.rs:96`), which only returns variables mise is told to redact.
   - A value written by Set value (`NAME = "value"`, no redact setting) is therefore no longer masked after a restart, for example in an agent's Inbox notice that quotes it.
   - The value also sits in plain text in the worktree, where the space's own agents can `cat` it.
   - **Fix:** remember which variable names were set through the prompt and add them back to the redactor on startup by reading the space's mise file, or have `set_space_var` mark them for redaction. Reword the UI text to say what is actually masked: control responses and Inbox previews.

6. **The control socket answers before any secrets are known.** `workspace.rs:233-239`
   - `start_control` runs before `check_mise`, and the mise check runs in the background. Until it finishes, `inbox.list` returns saved items (agent notices, plan tasks and rationales) with an empty redactor, so nothing is masked.
   - **Fix:** queue `inbox.list` while the first mise check is still running (the same way `PendingDraft` waits for the index), or answer it with a conflict error until the redactor is ready.

7. **The reviewed plan can still be edited while it launches, and the stored plan can differ from what ran.** `inbox_view.rs:208-221` (`add_plan_repo`), `:197-202` (`edit_reviewed_plan`)
   - The card disables the shape pills and the Drop buttons while busy, but the "Add a repo" input still works, and `edit_reviewed_plan` does not check `plan_launching`.
   - After the launch finishes, `inbox.approve(&id)` marks as approved a draft that includes repos that were never launched.
   - **Fix:** return early from `edit_reviewed_plan` and `add_plan_repo` while `plan_launching`, or record the draft that was actually launched when approving.

8. **A plan can be launched twice.**
   - **Plan stays pending after a successful launch.** If `inbox.approve` fails after the launch succeeded (`inbox_view.rs:243`, for example the Inbox file cannot be written), the error is shown but the plan stays pending. Approving it again creates a second space.
     - **Fix:** mark the plan as launching or approved in memory before calling `start_launch`, and only undo that if the launch fails.
   - **Second click before the redraw.** A second click on Approve before the card redraws reaches `approve_plan` again. `take_store` then fails and line 248 sets `plan_launching = false` while the first launch is still running.
     - **Fix:** add `if self.plan_launching { return; }` at the top of `approve_plan`.

## Suggestions

- `inbox_view.rs:240`: the success callback clears `plan_review` even when the user has moved on to review a different plan. Only clear it when `plan_review == Some(id)`.
- Every socket client is recorded as `Source::Agent`, so plans from the Parent Agent are labelled "an agent" and `Source::ParentAgent` is never used.
- `status.set` with level warning or error replaces any error banner (`control.rs:367`). An agent can hide a real failure such as a failed push, or show a convincing fake message. Prefix agent banners (for example "agent:") or never let them replace an app error.
- `plan_gate.rs:196`: the repo path in the Plan Gate card is truncated. An agent can name any git repository on disk (`draft_for` uses `repo_for_draft`), so show the full path, or flag repos outside the indexed folders.
- `save_env_value` accepts an empty value and marks the item resolved. It should reject an empty value.
- The Set value input shows the secret in plain text on screen.
- `reshape` through the keyboard now shows "store busy" as info instead of an error (`show_op_error` maps Conflict to info). This is the only visible change to M1 keyboard behaviour I found. Split, close, focus and zoom by keyboard behave as before.

## Summary

I found no critical issues in this area. What is already correct:
- Nothing on the socket can launch or approve: `space.draft` only posts a Plan Gate.
- Plan Gate items only offer Review in the Inbox panel.
- The card disables Approve and Reject while a launch runs.
- Inbox and plan previews are masked.
- Control layout changes only refocus the space being shown.
- `pane.split repo=` is checked against the space's repos.

The main risks:
- Control requests take keyboard focus out of dialogs (1).
- `pane.split kind=agent` resumes a running agent's session in a second `pi` process (2).
- Socket requests can silently open closed spaces and start their agents (3).
- The Parent Agent's "writes no code" is advisory, since it has a full shell and runs inside the data directory (4).
- Secret masking has gaps: values from Set value are no longer masked after a restart, and `inbox.list` answers before the first mise check (5, 6).
- Plan Gate launch ordering has edge cases, mainly edits during a launch and launching twice (7, 8).

I reviewed this by reading the code only; nothing was built or run.
