<!-- Wave 4 review of Milestone 2 (diff 5bb14a6..9e7034e), by a read-only pi reviewer. Findings are unverified until fixed or dismissed in docs/m2.md. -->

## Critical (must fix)

None. Every Inbox state change goes through `commit`, which works on a clone and only keeps it once `write_atomic` succeeds. Gates can only be approved or rejected, a Plan Gate with no repos cannot be approved, and each item settles exactly once. Over the socket, agents can only post `Notice` items. Placeholders exist only in the mise child's environment. I found no `unwrap` or `expect` on runtime-fallible calls.

## Warnings (should fix)

1. **A value typed into the Inbox is not masked after a restart.** `crates/xctx-core/src/mise.rs:104`
   - `set_space_var` writes `NAME = "value"` as a plain variable.
   - `save_env_value` (`xctx/src/inbox_view.rs:333`) adds it to the in-memory `Redactor`, but that is lost on quit.
   - On the next launch `MiseRunner::inspect` only reports variables mise marks for redaction. The value the user typed (usually a credential such as `STRIPE_KEY`) is never masked again in Inbox previews or `inbox.list`, unless the repo's own config happens to redact it.
   - **Fix:** write `NAME = { value = <toml_string>, redact = true }`. The existing `"{name} ="` prefix filter still replaces it. Add a test that `inspect` lists it in `secrets`.

2. **Pending items have no limit, so an agent can fill the Inbox.** `crates/xctx-inbox/src/store.rs:173`, `:393`
   - `prune` only drops settled items. Any agent in a pane can call `inbox.post` as often as it likes.
   - Each call adds a pending notice that is never dropped and rewrites and fsyncs the whole `inbox.json`. Memory, disk and per-post cost grow without limit, and the badge count becomes useless.
   - **Fix:** cap pending `Source::Agent` notices (for example 100, or per space). Either reject the post with a new `InboxError::Full`, or auto-dismiss the oldest pending agent notice in `commit`/`prune`.

3. **A dismissed missing variable comes back on every check.** `store.rs:358-372`
   - `sync_missing` only looks for *pending* items when deduplicating. The app runs `check_mise` for every space at startup (`workspace.rs:239`), so a variable the user dismissed is posted again as a new item each launch.
   - That contradicts `dismiss`'s "put away without acting on it".
   - **Fix:** treat a `Dismissed` item with the same `dir`, `mise_env` and name as suppressing a new post (still return no id for it), or document that dismissing only lasts until the next check.

4. **Previews are unmasked until the first mise check finishes.** This is outside my area (`xctx/src/workspace.rs:217`, `control.rs:580`), but it affects Inbox redaction.
   - The redactor starts as `Redactor::default()` and is only filled when the background `check_mise` completes. Until then, `inbox.list` shows notices containing secrets unmasked.
   - Repos whose inspection fails (`Err(_) => continue`) never contribute secrets at all.
   - **Fix:** either hold or flag `inbox.list` until the first check completes, or say in docs/m2.md that masking is best-effort for repos mise can inspect.

## Suggestions

- **`redact.rs:94`:** `redact_json` skips `Number`/`Bool` values and object keys, so a numeric secret such as `123456` in a number field leaks. It has no callers yet. Before control responses use it, mask numbers whose text equals a secret, and redact keys too.
- **`redact.rs:66-71`:** replacing one secret at a time can leave part of an overlapping secret. With `A="abcdef"` and `B="defghi"`, `"abcdefghi"` becomes `[REDACTED:A]ghi`. Finding all match ranges first, merging them, then replacing would fix this.
- **Whitespace breaks matching:** `SpaceDraft::new` rebuilds the title with `split_whitespace().join(" ")`. A multi-line or double-spaced secret in a task then no longer matches in the Plan Gate title. Secrets under 4 characters are never masked in text either; that is documented, but worth stating in docs/m2.md.
- **`mise_env.rs:190-222`:** the variable name parsed from stderr is not checked before `command.env(&var.name, …)`. A name with `=` or NUL would set the wrong variable or fail to spawn. Require `mise::is_env_var_name(name)`, or return `None`.
- **`mise_env.rs:95-109`:** set `NO_COLOR=1` (and/or `MISE_COLOR=0`) on the command. A user's `CLICOLOR_FORCE` or `FORCE_COLOR` would add ANSI codes. The `Version:` and `Run with --verbose` filters would then miss, and that noise would end up in the help text shown in the Inbox.
- **`mise_env.rs:110`:** `output()` has no timeout. A hanging `_.source` hook or slow plugin keeps a background thread busy for each of up to 64 reruns. Use a wait-with-deadline (spawn, poll `try_wait`, kill).
- **`tests/mise_env.rs:13-24`:** isolation is good but not complete. Also set `HOME` to the temp home, `MISE_SYSTEM_CONFIG_FILE`/`MISE_SYSTEM_DIR` to temp paths, and remove `MISE_OVERRIDE_CONFIG_FILENAMES`/`MISE_ENV_FILE`. Otherwise `/etc/mise` or a user's exported `MISE_*` variables can change the results.
- **`store.rs` (inbox) `open`:** `next_id` is trusted as stored. If it is below an existing `inbox-<n>`, for example after a hand edit, ids repeat and `get` returns the first match. Set `next_id` to at least the highest parsed id + 1 on load.
- **`sync_missing`:** it rewrites and fsyncs `inbox.json` even when nothing changed, once per repo per check. Skip `write_atomic` when the data is unchanged.
- **`approve`:** a `SpaceWiden` with no repos and `production: false` ("no change") can be approved. Reject it the way an empty plan is rejected.
- **`xctx-core/src/store.rs:62-63`:** the doc says prototype files have "the same `roots` and `spaces`". The Swift `Space` has `name` and `created` (an ISO date) and a `new` shape, so a prototype file with any spaces fails to parse. The app does show that error. Reword the doc to say only empty prototype `spaces` are readable.

## Summary

There are no critical issues in the Inbox or the core redaction and mise code. The state transitions are correct, and a change that cannot be written is not kept. All the tests I checked (`xctx-inbox/tests/inbox.rs` and `xctx-core/tests/mise_env.rs`) use temp directories, and the mise tests isolate mise's config, data, state, cache and trust list. Writes go through `NamedTempFile`, so `inbox.json` and the space mise file are created with mode 0600.

The main gaps are these:
- A secret typed into the Inbox loses its masking after a restart.
- Agents can post pending notices without limit.
- Dismissed missing variables come back on every check.
- At startup, previews are unmasked until the first mise check finishes.

I read the code but did not build or run anything.
