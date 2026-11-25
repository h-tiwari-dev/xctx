# xctx

xctx is an agent multiplexer for macOS: a native desktop app built with Rust and GPUI where you describe a task and it opens a space with a worktree per repo, agents, shells, and plugin panes. See [`design/README.md`](design/README.md) for the full vision.

**Status:** Milestone 1 is complete — Home, repo indexing & task drafting, per-space git worktrees & `mise.<slug>.toml`, truecolor PTY terminal (`pi`, `nvim`, `lazygit`), split/zoom/reshape panes (`Dev`/`Debug`/`Review`), layout persistence & agent resume (`pi --continue`), and Ship/Discard all work end-to-end. See [`docs/roadmap.md`](docs/roadmap.md) and [`docs/m2.md`](docs/m2.md) for what comes next.

**Milestone 2 (complete):** xctx is now an agent-drivable harness (all 4 waves implemented, reviewed, and hardened):

- **Control socket and `xctx-ctl`.** Agents and scripts can list spaces, read and change layouts on open spaces, draft spaces and post to the Inbox. Every pane gets `XCTX_CONTROL_SOCKET` and has `xctx-ctl` on its `PATH`.
- **Parent Agent on Home.** An `omp` session in a drawer running outside the data directory, told its advisory no-code role and given only its read and bash tools. It drafts spaces through `xctx-ctl space.draft` and cannot launch anything.
- **Inbox on Home.** The status bar shows how many items are pending. The Inbox holds four kinds of item:
  - **Plan Gates.** A new draft opens as a card on Home under the Parent Agent (or Review opens it from the Inbox), where you change the shape, drop repos or add them (click a suggested repo, type part of a name and press Enter, type a folder, or **Choose folders…** for several at once), then Launch (⌘↵) or Reject.
  - **Missing mise variables.** Each comes with mise's own help text. Set value writes `{ value = ..., redact = true }` into the space's git-ignored `mise.<slug>.toml`.
  - **Notices** from agents (bounded cap).
  - **Space widenings** are shown, but approving them is not supported yet.
- **Secret redaction.** Values mise marks as secret are masked as `[REDACTED:NAME]` in control responses and Inbox previews (including after restarts).

**Milestone 3 (complete):** multi-agent graphs with an orchestrator, one writer per repo and read-only helpers, a shared contract and channel, write → test → probe → review loops with autonomy levels, and space widening. See [`docs/m3.md`](docs/m3.md).

**Milestone 4 (complete):** git-branch a writer agent into competing attempts, each with its own worktree, `PORT` and forked `pi` session. Compare them on a review sheet (⌘⇧R), adopt the winner and discard the rest. Fork with ⌘⇧F. See [`docs/m4.md`](docs/m4.md).

**Milestone 5 (complete):** plugins are live panes and agent tools. ⌘⇧P opens a space's Plugins menu, which mounts
first-party plugins (terminal, logs, API client, database, browser, Jira, Figma) and plugins from user or repo
`plugin.toml` manifests (repo plugins need trust first). Agents call a plugin's tools over the harness, and every
write waits for approval in the Inbox. A plugin's events can start or stop the space's loop. See
[`docs/m5.md`](docs/m5.md).

No further milestone is planned yet; see [`docs/roadmap.md`](docs/roadmap.md).

**Agents.** Agent panes run `omp` by default (the built-in `omp` runner: a task, and `omp --continue` on reopen). omp cannot create a session under an id xctx chooses, so spaces on the default runner get no Fork or attempts; for those, set `"runner": "pi"` on the space (or on one agent) in `spaces/<slug>/graph.json` in the data directory. When an agent exits on its own (you quit it), its pane opens your login shell in the same worktree with the space's environment, and the pane header offers **Restart agent**.

**Living spaces.**
- **Home** lists every space's agents with what each one's pane is doing (LIVE, EXITED, NOT STARTED); **Open** jumps to an agent's pane and **Start** opens one. The folders xctx indexes for repos sit above the Parent Agent.
- **The Parent Agent drives them** through `xctx-ctl agent.list`, `agent.read space=<slug> agent=<id>` (an agent's screen, secrets masked) and `agent.send space=<slug> agent=<id> text="..."` (typed into the agent's terminal, shown in the status bar).
- **In a space**, every agent card in the graph panel (⌘G) has **Open** or **Start**. New lanes (the orchestrator, helpers, the writer of an added repo) start with a brief of their role instead of a bare prompt. Closing an agent's pane keeps the agent: **+ Agent** in the tab bar lists the agents with no pane (**Start** brings one back) and adds helpers. **+ Repo** (tab bar or the graph's Writers row) adds as many repos as you pick in one go, each with its own worktree, writer and agent pane: click repos in the list, type to search and press Enter for the top match, or **Choose folders…** (a folder of repos adds each one in it).
- **The browser plugin** is a live WebKit view in its pane: browse any site there (a bare host like `google.com` gets `https://`), with Back, Forward, Reload, a loading badge, the console and failed requests in a strip at the bottom, and Web Inspector on right-click. Agents drive the same view with its tools (`page.open`, `page.click`, `page.type`, `page.text`, `page.screenshot`, `page.console`), so you watch them work; the tools only open and read the space's own app (`localhost:$PORT`) and refuse a page you browsed elsewhere.
- **Helpers** include **Browser**, which tests the app in xctx's own browser plugin (`page.open`, `page.click`, `page.type`, `page.text`, `page.screenshot`, `page.console`) and never in an outside browser.
- **Pane headers** show the repo's git host address (click copies it), its branch, **Files** (the in-app file explorer of the worktree) and **Git** (lazygit in it).
- **Launch** sets up web apps: the first repo whose `package.json` names a web framework and a `dev` or `start` script gets a terminal pane running that script on the space's `PORT` (installing dependencies first when `node_modules` is missing) and xctx's browser on it. The space's other repos (a backend beside a frontend) get the API client.

## Prerequisites

- macOS 13.0 or later
- Xcode (or Xcode Command Line Tools)
- [mise](https://mise.jdx.dev/)

Install the pinned Rust toolchain (`rustfmt` and `clippy` included) via mise:

```sh
mise trust
mise install
```

## Tasks

All development runs through mise tasks:

| Task | Description |
| --- | --- |
| `mise run dev` | Run the `xctx` app in debug mode, with `xctx-ctl` built next to it for its panes |
| `mise run ctl <method> [key=value]...` | Run `xctx-ctl` against the running window (see below) |
| `mise run build` | Build the `xctx` app and `xctx-ctl` in release mode |
| `mise run bundle` | Assemble the release build, with `xctx-ctl`, into `xctx.app` at the repo root |
| `mise run test` | Run tests across the workspace |
| `mise run fmt` | Format Rust code across the workspace |
| `mise run fmt:check` | Check Rust formatting across the workspace |
| `mise run lint` | Run Clippy across the workspace and all targets (`-D warnings`) |
| `mise run ci` | Run `fmt:check`, `lint`, and `test` together |
| `mise run clean` | Remove Cargo build artifacts and `xctx.app` |

## Driving xctx with `xctx-ctl`

`xctx-ctl` lets agents and scripts inspect and rearrange a running window over its control socket. The socket uses JSON-RPC 2.0, one JSON message per line.

```sh
mise run ctl help                                  # every method and its parameters
mise run ctl ping                                  # is a window running?
mise run ctl space.list
mise run ctl layout.get space=rate-limit
mise run ctl pane.split space=rate-limit axis=vertical kind=shell
mise run ctl space.draft task="add rate limiting" repos=api,web
mise run ctl -- --compact inbox.list all=true      # put options after --
```

- Arguments are `key=value`. `pane` is a number, `all` a boolean, and `repos` a comma-separated list. Use `key:=<json>` to pass any JSON value.
- The socket is `$XCTX_CONTROL_SOCKET`, else `control.sock` in the data directory (`$XCTX_DATA_DIR`, else `~/Library/Application Support/xctx`). Use `--socket <path>` to pick another one.
- Inside a space's pane, `space=` defaults to `$XCTX_SPACE`, so `xctx-ctl pane.split axis=vertical` acts on the space you are in.
- `xctx-ctl` prints the result as JSON. Exit status: `0` ok, `1` the window refused the request, `2` bad arguments, `3` no window is running.
- In a pane of xctx, `xctx-ctl` is already on `PATH`, and `XCTX_CONTROL_SOCKET` points at its window.
- A request about a space that is not open yet opens its tab first, which starts its panes. `layout.get` is the exception: it reads the saved layout and starts nothing.
- The Parent Agent runs in `<data dir>-parent-agent` beside the data directory and resumes its session there (`omp --continue`). Its model and billing are whatever your `omp` is set up to use.
- Nothing on the socket can approve or launch. `space.draft` posts a Plan Gate to the Inbox and shows it on Home under the Parent Agent, and the space launches only when you press Launch (⌘↵) in the window.

### Trying it without touching your spaces

Run the app on a throwaway data directory, then point `xctx-ctl` at that window's socket:

```sh
XCTX_DATA_DIR=/tmp/xctx-try mise run dev
XCTX_CONTROL_SOCKET=/tmp/xctx-try/control.sock mise run ctl inbox.list   # in another terminal
```

## Tests

`mise run ci` runs everything. To run one area:

```sh
mise exec -- cargo test -p xctx-control                  # control socket, client and CLI
mise exec -- cargo test -p xctx-inbox                    # Inbox and Plan Gate
mise exec -- cargo test -p xctx-core --test mise_env     # mise required variables and secrets
```

Tests use temporary directories and sockets, and an isolated mise. They never touch your real data, repos or mise trust list.
