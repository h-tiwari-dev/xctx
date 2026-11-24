# xctx

xctx is an agent multiplexer for macOS: a native desktop app built with Rust and GPUI where you describe a task and it opens a space with a worktree per repo, agents, shells, and plugin panes. See [`design/README.md`](design/README.md) for the full vision.

**Status:** Milestone 1 is complete — Home, repo indexing & task drafting, per-space git worktrees & `mise.<slug>.toml`, truecolor PTY terminal (`pi`, `nvim`, `lazygit`), split/zoom/reshape panes (`Dev`/`Debug`/`Review`), layout persistence & agent resume (`pi --continue`), and Ship/Discard all work end-to-end. See [`docs/roadmap.md`](docs/roadmap.md) and [`docs/m2.md`](docs/m2.md) for what comes next.

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
| `mise run dev` | Run the `xctx` app in debug mode |
| `mise run build` | Build the `xctx` app in release mode |
| `mise run bundle` | Assemble the release build into `xctx.app` at the repo root |
| `mise run test` | Run tests across the workspace |
| `mise run fmt` | Format Rust code across the workspace |
| `mise run fmt:check` | Check Rust formatting across the workspace |
| `mise run lint` | Run Clippy across the workspace and all targets (`-D warnings`) |
| `mise run ci` | Run `fmt:check`, `lint`, and `test` together |
| `mise run clean` | Remove Cargo build artifacts and `xctx.app` |
