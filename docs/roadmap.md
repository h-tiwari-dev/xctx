# Roadmap

xctx is built in milestones toward the living, task-first workspace described in [`design/README.md`](../design/README.md).
Each milestone ships a usable vertical slice, organized into UI-free foundation crates, GPUI view crates, app
integration, and review/hardening waves.

## Milestones

### Milestone 1 — Build xctx inside xctx (complete)
See [`docs/m1.md`](m1.md).
- Home screen with living spaces, indexed repository roots, and task prompt.
- Task drafting with repo match reasons, add/drop repos, and explicit Launch gate.
- Per-space git worktrees (`space/<slug>`), git-ignored `mise.<slug>.toml` with unique `PORT` and `MISE_ENV=<slug>`,
  and atomic on-disk store (`XCTX_DATA_DIR`).
- Truecolor PTY terminal (`xctx-term` + `xctx-term-view`) supporting `pi`, `nvim`, and `lazygit` (Kitty keyboard
  protocol / `Shift+Enter`, Alt/Ctrl chords, scrollback, selection/copy, bracketed paste, mouse reporting).
- Split pane tree (`xctx-layout` + `xctx-ui`) with drag-resizing, focus, zoom, and non-destructive `Dev`/`Debug`/`Review`
  reshaping that preserves live agent panes.
- Quit/reopen persistence (`pi --continue`), Ship (push + `gh pr create`), and Discard (remove worktrees, keep branches).

### Milestone 2 — Local control socket, Parent Agent & Inbox (complete)
See [`docs/m2.md`](m2.md).
- Local Unix domain control socket (`xctx-control`) so agents can inspect and drive the window (`space.*`, `layout.*`,
  `pane.*`, `status.*`).
- Always-on **Parent Agent** (a non-coding `pi` session connected to the control socket and `RepoIndex`) that turns
  tasks into spaces, reshapes panes, and owns the Inbox.
- **Inbox & Plan Gate**: approvals for launching or widening a space (new repos, agents, budget, or `MISE_ENV=production`).
- **Mise environment hardening**: detect missing `required` env vars and route them to the Inbox with mise's help text;
  redact secret values in shared output.

### Milestone 3 — Multi-agent graphs, shared contracts & autonomous loops (in progress)
See [`docs/m3.md`](m3.md).
- **Orchestrator per space** + **one writer per repo** + read-only helpers (test runner, reviewer).
- **Shared Space Store & Channel**: contract (API/schema/event shape), findings, shared notes, and `/tmp` scratch
  artifacts so writers coordinate without editing each other's repos.
- **Structured loops & autonomy levels**: `write -> test -> probe -> review` with *ask each step*, *within budget*,
  and *until checks pass*.
- **Agent Graph View**: live GPUI view to inspect the graph, pause/resume agents, add/remove lanes, talk directly to a
  specific agent, or split/merge spaces.

### Milestone 4 — Branching agents & competing attempts
- **Git-branch an entire agent** (hidden worktree + `pi --fork` session tree + scratch state) to run competing
  attempts (Solutions 1, 2, 3) in parallel.
- **Outcome comparison & review sheet**: compare repros, tests, and diffs side by side, discard dead ends, or merge the
  winner.

### Milestone 5 — Plugins & connectors as live panes + agent tools
- **Plugin manifest contract**: every plugin provides a live GPUI pane, agent tools over the harness, and events that
  trigger or stop loops.
- **First-party plugins**: Browser pane, API client (Postman-style replay), Logs & read-only DB pane (write-gated),
  Jira/Figma connectors, pluggable agent runners (`pi`, `fx`), and voice/canvas task entry.
