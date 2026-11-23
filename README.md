# xctx

A native macOS workspace for agentic coding. You type a task; xctx finds the repos it touches in the folders you
index, and opens a **space**: a worktree per repo, a `pi` agent per repo, shells, git and a browser in split panes.
Spaces reshape (dev, debug, review), ship (push + PR) and dissolve.

## Run

```sh
./build-app.sh && open xctx.app
```

Requires Xcode 26 / Swift 6. Agents run `pi` (`@earendil-works/pi-coding-agent`); `lazygit`, `nvim`, `mise` and
`gh` are used when installed.

## Spaces

- **Index** folders on Home; only git repos inside them are read, on this Mac.
- **Type a task** in the prompt bar. The draft lists the repos it found and why. Nothing runs until **Launch** (⌘↩).
- **Launch** makes a worktree per repo on `space/<id>`, a `mise.<id>.toml` (git-ignored) with the space's own `PORT`,
  and an agent per repo told about its task, repo and the harness.
- **Shapes**: dev (agents over shells), debug (agents beside shells), review (lazygit per repo): ⌥⌘1 ⌥⌘2 ⌥⌘3.
  Reshaping keeps the same agents.
- **Ship** (⇧⌘S) pushes and opens PRs where there are new commits and a remote, then removes the worktrees.
  **Discard** removes the worktrees and keeps the branches.

## Harness

Agents drive the window from their terminals:

```sh
"$XCTX_CTL" ctl pane.mount kind=browser url=http://localhost:$PORT split=right
"$XCTX_CTL" ctl gate.ask title="run a migration" detail="orders table"
```

Commands: `spaces.list index.search space.propose space.launch space.shape space.open pane.list pane.mount
pane.close pane.send pane.read page.open gate.ask gate.status`. One JSON object per line over
`~/Library/Application Support/xctx/control.sock` (owner-only).

## Test

```sh
swift test       # core: git, index, spaces, pane tree
./selftest.sh    # drives the real window against throwaway repos, with `cat` as the agent
```

Environment: `XCTX_DATA_DIR`, `XCTX_INDEX_ROOTS`, `XCTX_AGENT_CMD`, `XCTX_SELFTEST`.
