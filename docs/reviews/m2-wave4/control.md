<!-- Wave 4 review of Milestone 2 (diff 5bb14a6..9e7034e), by a read-only pi reviewer. Findings are unverified until fixed or dismissed in docs/m2.md. -->

## Critical (must fix)

None found. Peer-uid checking, owner-only socket mode, refusing to delete non-socket files, the per-line size limit and leaving a replaced socket alone at shutdown are all correct.

## Warnings (should fix)

1. **Two windows starting at once can delete each other's socket.** `crates/xctx-control/src/server.rs:200-216, 139-140`
   - `clear_stale_socket` probes with `connect`, then calls `remove_file`, and only later does `bind` run. Nothing stops two processes interleaving:
     1. A and B both find the stale socket dead.
     2. A removes it and binds.
     3. B's `remove_file` then deletes A's live socket and B binds.
   - A keeps serving a listener nobody can reach and reports success. Its `Drop` sees the inode changed and won't clean up, so the failure is silent.
   - This is reachable with two `mise run dev` runs, or a relaunch while the old window is still exiting.
   - **Fix:** hold an exclusive `flock` on a sibling lock file (`control.sock.lock`) from before `clear_stale_socket` until the `Server` is dropped. If the lock is taken, return `AlreadyRunning`.

2. **`xctx-ctl` cannot pass a plain string containing `:=`.** `crates/xctx-control/src/cli.rs:121-126`
   - `split_once(":=")` runs before `split_once('=')`, so it matches the first `:=` anywhere in the argument.
   - Example: `message=ratio:=1` becomes key `message=ratio` with JSON value `1`, which fails as an unknown field. `task=set x:=y` fails with "is not JSON".
   - Agents writing code-like task text will hit this.
   - **Fix:** split on the first `=`. If the key ends with `:`, strip the `:` and parse the value as JSON; otherwise treat it as a string.
     ```rust
     let (lhs, raw) = arg.split_once('=').ok_or(...)?;
     if let Some(key) = lhs.strip_suffix(':') { /* json */ } else { /* typed string */ }
     ```

3. **If spawning a connection thread fails, the socket is leaked and the client hangs.** `crates/xctx-control/src/server.rs:258-272`
   - The `try_clone` copy is put in `connections` before the spawn.
   - When `spawn` fails, the closure (and `stream`) is dropped, but the clone stays in the map. That keeps the socket open and the entry is never removed.
   - The client gets neither an answer nor EOF, and waits for its 35 s timeout. Each failure leaks one fd until the server is dropped.
   - **Fix:** on `Err`, remove `id` from `connections` and call `shutdown(Both)` on that copy.

4. **A server-side timeout does not cancel the request.** `crates/xctx-control/src/server.rs:401-410`
   - When the deadline passes, the `Timeout` reply goes out, but the `Incoming` is still queued in the app (`crates/xctx/src/control.rs:281`) and runs later.
   - Example: the client sees `pane.split` time out, retries, and gets two splits.
   - The error text ("did not answer") suggests nothing happened.
   - **Fix (either):**
     - Expose `Responder::is_cancelled()` (check whether the receiver is gone, e.g. via a shared `AtomicBool` set on timeout). The app then skips stale mutating calls.
     - At minimum, make the message say the action may still happen, and document this on `ServerConfig::reply_timeout`.

5. **The client never checks who owns the server socket.** `crates/xctx-control/src/client.rs:30-38`
   - The server checks its peer's uid; the client does not. If `XCTX_CONTROL_SOCKET` or `--socket` points somewhere shared (e.g. `/tmp/...`), another local user can create that socket first and receive the client's requests (task text, inbox bodies). It can also send back made-up results.
   - **Fix:** after `connect`, call `getpeereid` and refuse with `ControlError::Protocol` (or a new variant) unless the uid equals `geteuid()`. Move the existing `peer_is_same_user` helper into a shared module so both sides use it.

## Suggestions

- **No limits on connections or idle time.** `server.rs:252-269, 329-339`
  - There is one thread per connection with no cap, and no read timeout on connection streams. A buggy or hostile agent in a pane (same uid, so it passes the peer check) can open thousands of idle connections, each holding a thread and up to 1 MiB of line buffer.
  - Cap concurrent connections (e.g. 64; refuse the rest with `shutdown`).
  - Set an idle `set_read_timeout` (e.g. 10 min) so abandoned connections are reclaimed.
- **Notifications hold the connection for up to `reply_timeout`.** `server.rs:379`
  - For a notification, `dispatch` still blocks on `rx.recv_timeout`, so the next request on that connection waits up to 30 s.
  - When `envelope.id.is_none()`, hand the call to the handler and return without waiting.
- **The client accepts a success reply with a `null` id.** `client.rs:103-111`
  - The null-id exception is meant for server parse errors, but it also lets through a reply that has `null` id and a `result`.
  - Only accept a `null` id when the outcome is `Outcome::Error`.
- **The client is unusable after its own read timeout.** `client.rs:72-89`
  - After a timeout, the late reply is read by the next `call` and fails as an id mismatch.
  - Either document that the `Client` must be dropped after a timeout, or mark it broken and refuse further calls.
- **Exit code 3 is used for more than "no window".** `bin/xctx-ctl.rs:59-62`
  - Every non-`Rpc` error exits 3, which the help text calls "no window". That includes client-side timeouts, protocol errors and other I/O errors.
  - Use 3 only for `NotRunning` and `NoSocketPath`, and 1 (or a separate code) for the rest.
- **A busy server's socket can be deleted at startup.** `server.rs:212`
  - On macOS, `connect` to a listener whose backlog is full fails with `ECONNREFUSED`, so a live but busy server looks dead and its socket gets removed.
  - This is very unlikely with a 100 ms accept poll. The lock file from Warning 1 covers it as well.

## Summary

I found no critical issues in `crates/xctx-control`; this is from reading the code only, since I didn't build or run anything.

- **Covered well:** JSON-RPC handling (notifications, `null` vs absent ids, batches rejected, error codes), owner-only socket mode with a peer-uid check, the 1 MiB line limit, shutdown without hangs, and replaced-socket handling.
- **Rules:** the tests use temporary directories only, there are no `unwrap`s on runtime failures, public items have doc comments, there is no GPUI dependency, and all dependencies have permissive licences.
- **Should fix:**
  - A race between two windows starting at once that can silently orphan one window's socket.
  - A CLI parsing bug for string values containing `:=`.
  - An fd leak and client hang when a connection thread fails to spawn.
  - Requests that time out still running later, so a retry can split twice.
  - The client not checking the server's uid.
