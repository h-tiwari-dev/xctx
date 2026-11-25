# Working in this repo

xctx is a native macOS agent multiplexer written in Rust with GPUI. The vision is in `design/README.md`, the multi-milestone
plan is in `docs/roadmap.md`, and Milestones 1 to 5 are complete (`docs/m1.md` to `docs/m5.md`); no further
milestone is planned yet. Read these before changing anything. `docs/m2.md` ends with notes for
working here: keeping tests away from the user's mise config, the Swift prototype, and taking screenshots of the window.

## Commands

Everything runs through mise. Run `mise trust && mise install` once in a new checkout or worktree.

| Command | Use |
| --- | --- |
| `mise run ci` | format check, clippy with warnings as errors, and all tests. Must pass before every commit. |
| `mise run dev` | run the app in debug |
| `mise run fmt` | format |
| `mise run bundle` | build `xctx.app` |

Call `cargo` through mise (`mise exec -- cargo ...`); there is no other Rust toolchain on the machine.

## Layout

- The workspace is every directory under `crates/`. A new crate needs no change to the root `Cargo.toml`.
- A crate declares its own dependencies in its own `Cargo.toml`.
- Only `crates/xctx` (the app) and crates whose name ends in `-view` or `-ui` may depend on GPUI. Logic that can be
  tested without a window goes in a crate that does not.
- GPUI comes from the Zed repository, pinned to one release tag in `crates/xctx/Cargo.toml`. Every crate that uses
  GPUI pins the same revision. Its API changes between releases, so read the source at the pinned revision (Cargo
  checks it out under `~/.cargo/git/checkouts/`) instead of relying on memory or on docs for another version.

## Rules

- Dependencies must be under a permissive licence (MIT, Apache-2.0, BSD, ISC, Zlib, MPL-2.0). No GPL or AGPL.
- Zed's `gpui*` crates are Apache-2.0 and may be read and used. Most other Zed crates, including `terminal` and
  `terminal_view`, are GPL: do not copy or adapt code from them.
- The Swift prototype this repo replaced is in git history at commit `413b816` (`git show 413b816:Sources/...`). Use
  it as a reference for behaviour, not as a design to port line by line.
- Tests must not touch the user's real data or repositories. Use temporary directories, and `XCTX_DATA_DIR` for
  anything that would otherwise write to `~/Library/Application Support/xctx`.
- No `unwrap` or `expect` on anything that can fail at runtime because of the outside world (processes, files, git,
  user input). Return errors the app can show.
- Public items carry a doc comment that says what a caller needs to know.
- Do not push, and do not commit to `main`, unless the task says so.
