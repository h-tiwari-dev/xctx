# Milestone 5 Wave 4: review findings

Three read-only reviewers ran on `git diff 5f3e5e9..7232f4b`, with the prompt in
[`../m2-wave4/reviewer-prompt.md`](../m2-wave4/reviewer-prompt.md). They read the code only; nothing was built or run.
The pane UI (`m5-app-ui`) was merged after the review.

## Plugin and connectors (`xctx-plugin`, `xctx-connectors`)

**Critical**
- P1. A Postgres connection URL can turn read-only off: `?options=-c default_transaction_read_only=off`, or
  `?service=`, overrides `PGOPTIONS`. Strip or refuse `options` and `service`, and set read-only in the URL itself, or
  run `SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY` first.
- P2. `dblink` gets past read-only: a `SELECT dblink_connect(..), dblink_send_query(.., 'DROP ...')` writes through a
  session that is not read-only. Refuse any word starting with `DBLINK`, `LO_`, `PG_LS_` or `PG_READ_`.

**Warnings**
- P3. Quoted function names (`"pg_read_file"(...)`) skip the deny lists. Check the content of quoted identifiers too.
- P4. SQLite comments do not nest, but the guard lexes them as nested, so a statement that looks commented out can
  still run. Nest comments only for Postgres.
- P5. Setter PRAGMAs in the parenthesised form (`PRAGMA journal_mode(WAL)`) pass the guard. Allow arguments only for
  the `*_info`, `*_list` and `foreign_key_check` pragmas.
- P6. Processes a tool starts can outlive the call (`cmd &`, `nohup`), and the reader threads can leak. Always kill the
  process group once the drain ends, and handle pgid reuse.
- P7. When output is cut at the cap, part of a secret can show where the cut falls. Cut the raw bytes safely before
  masking.
- P8. `shell.run` is marked `read` but runs any program. Mark it `write`, or document it as an unrestricted exec.
- P9. Repo trust is checked against a second read of `plugin.toml`, which may differ from the one parsed. Hash the
  bytes that were parsed, and expose that hash.
- P10. The JSON a built-in returns is not capped. Cap it and set `truncated`.
- P11. Absolute `log_file` and `database` paths are trusted even when an agent supplied them. Accept them only from
  the user, or gate them.

**Suggestions**
- Document option injection (values starting with `-`); require https for Jira and Figma except on loopback; force
  `standard_conforming_strings=on`; settle production scope for the built-ins; refuse FIFOs in `logs.query`; consider a
  joined `--flag=task` style for runners; declare the `times` parameter of `http.replay`.

## HTTP, browser, layout and control (`xctx-http`, `xctx-browser`, `xctx-layout`, `xctx-control`, `xctx-inbox`)

**Warnings**
- H1. The host allow-list ignores ports, so `http.send` reaches every local HTTP service, including other spaces'
  apps and Chrome's DevTools endpoint. Allow `(host, port)` pairs: the base URL's, and the space's `PORT` on loopback.
- H2. Sensitive headers are dropped on a redirect only when the host changes, not on a scheme or port change, and a
  307/308 keeps the body. Compare `(scheme, host, port)`.
- H3. `http.json` stores credentials unmasked. Mask sensitive headers before saving, and keep raw values in memory for
  replay.
- H4. Any failed page action quits Chrome. Reset the browser only on transport errors.
- H5. `page.screenshot` and `page.console` do not check the page's current URL, so after a redirect or a click they
  capture any site. Check `page.url()` against the policy.
- H6. An orphaned Chrome (left when xctx dies) makes every later launch fail. Attach to it or kill it before
  launching.
- H7. A write gate's arguments are cut at 4000 characters for display, but the full arguments run. Refuse calls over
  the cap, or show everything.
- H8. `plugin.emit` (Propose) can `pass` or `fail` the loop, which `loop.advance` needs Layout for. Require Layout.
- H9. `mask_url` misses token-like usernames and query values (`token=`, `api_key=`), and event details use an unmasked
  URL.

**Suggestions**
- Add more credential headers (`x-auth-token`, `*-token`, `*-key`); declare `times` on `http.replay`; idle CDP
  backlog; signal the group before reaping; refuse a non-loopback `webSocketDebuggerUrl`; reset load tracking per
  navigation; keep damaged `http.json` files instead of overwriting them, and save the history under a lock.

## App (`crates/xctx`)

**Critical**
- A1. Trust is recorded for whatever `plugin.toml` holds at approval time, not for what the gate asked about. Store
  the hash when the gate is posted, refuse if it changed, and never trust without a recorded hash.
- A2. The file that is hashed is not the file that runs: the registry parses one read, and the trust callback hashes
  another. Use the registry's hash of the parsed bytes.

**Warnings**
- A3. Tools and tail commands inherit xctx's whole environment. Clear it, add back a small allowlist, then `PATH` and
  the plan's environment.
- A4. One call can fire unbounded events and advance the loop several stages. Apply one loop action per call
  (`loop_outcome`), collapse events by name, cap them, and keep elapsed time out of the debounce key.
- A5. A noisy failing log posts an Inbox notice every 5 seconds forever. Post only when the loop newly blocks, or rate
  limit much longer.
- A6. A write that never ran is still marked approved. Settle the gate only on success; otherwise leave it pending.
- A7. After a restart, a write can run against the wrong database pane. Record the pane and the masked target in the
  gate.
- A8. A tail line with no newline buffers without bound. Cap the line read.
- A9. An agent can start Chrome and plugin state for a space whose tab is closed. Refuse plugin calls, emits and pane
  actions for closed tabs.
- A10. Same as H8: `plugin.emit` with `pass` needs Layout.

**Suggestions**
- Trust covers only `plugin.toml`, not the scripts it runs; say so or hash the folder. Mask `base_url` and mount
  settings shown in the UI. Drop events from unmounted panes. Paste stdin tasks with bracketed paste. Deduplicate
  pending gates. Use the host's data directory for scratch. Remove browser profiles and `plugins.json` on Ship or
  Discard. Refuse an attempt relaunch on a runner that cannot fork.
