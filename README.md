# xctx

xctx is an agent multiplexer for macOS: a native desktop app built with Rust and GPUI where you describe a task and it opens a space with a worktree per repo, agents, shells, and plugin panes. See [`design/README.md`](design/README.md) for the full vision.

**Status:** Milestone 1 is complete — Home, repo indexing & task drafting, per-space git worktrees & `mise.<slug>.toml`, truecolor PTY terminal (`pi`, `nvim`, `lazygit`), split/zoom/reshape panes (`Dev`/`Debug`/`Review`), layout persistence & agent resume (`pi --continue`), and Ship/Discard all work end-to-end. See [`docs/roadmap.md`](docs/roadmap.md) and [`docs/m2.md`](docs/m2.md) for what comes next.

**Milestone 2 (in progress):** Wave 1 added the control socket and `xctx-ctl` CLI (`crates/xctx-control`), the Inbox and Plan Gate model (`crates/xctx-inbox`), and mise required-variable checks and secret redaction (`xctx-core`). Wave 3 connects the socket to the app:

- The app opens the control socket at startup and answers every method.
- Every pane gets `XCTX_CONTROL_SOCKET`, and has `xctx-ctl` on its `PATH`.
- The Inbox starts with a welcome notice.
- Required mise variables that are not set in a space's worktrees are posted to the Inbox, and mise's secret values are masked.

The Inbox panel, Plan Gate approval and the Parent Agent drawer come with Wave 2 (`m2-ui`). Until then, Plan Gates wait in the Inbox, and `xctx-ctl inbox.list` shows them.

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
- Nothing on the socket can approve or launch. `space.draft` posts a Plan Gate to the Inbox, and the space launches only when you approve it in the window.

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
