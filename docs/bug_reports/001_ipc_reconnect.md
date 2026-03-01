# Bug Fix: IPC Server Stops Accepting Connections After Client Disconnect

## Context

When a CLI or GUI client disconnects from the ingot-server (gracefully or by crash), the server's IPC command thread can no longer accept new connections. The server process stays alive but becomes unresponsive to all subsequent client requests, requiring a server restart.

**Root cause:** `ipc_command.rs` creates a single iceoryx2 `server` port at startup (line 64) and holds it for the entire process lifetime. When a client disconnects, the port's internal state may become invalid. The error handler (lines 93-96) logs and sleeps but never recreates the port, leaving the server permanently broken.

The publisher thread (`ipc_publisher.rs`) is unaffected — pub/sub is one-way broadcast with no connection state.

**Branch:** `fix/ipc-reconnect`
**Base:** `dev`

---

## Tasks

### 1. Recreate server port on receive error

**File:** `crates/ingot-server/src/ipc_command.rs`

Change `server` from immutable to mutable. On `Err` from `server.receive()`, drop the old port and recreate via `service.server_builder().create()`. Retry with backoff if recreation fails, respecting the shutdown flag.

```rust
// Line 64: make mutable
let mut server = service.server_builder().create()?;

// Lines 93-96: replace error arm
Err(e) => {
    warn!(error = %e, "IPC server receive error, recreating port");
    drop(server);
    loop {
        match service.server_builder().create() {
            Ok(new_server) => {
                server = new_server;
                info!("IPC server port recreated");
                break;
            }
            Err(recreate_err) => {
                error!(error = %recreate_err, "failed to recreate server port");
                if shutdown.load(Ordering::Relaxed) {
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
    }
}
```

### 2. Add integration test for sequential client reconnection

**File:** `crates/ingot-server/tests/server_integration.rs`

Add `test_server_accepts_new_client_after_disconnect`:
1. Spin up test harness (server + paper exchange)
2. Create IPC client A, send heartbeat, verify response
3. Drop client A (simulate disconnect)
4. Brief sleep for iceoryx2 cleanup
5. Create IPC client B, send heartbeat, verify response succeeds

This test would fail without the fix (client B's request would timeout or error).

---

## File Change Summary

| File | Action |
|------|--------|
| `crates/ingot-server/src/ipc_command.rs` | Mutable server port, recreation on error with retry loop |
| `crates/ingot-server/tests/server_integration.rs` | New test: `test_server_accepts_new_client_after_disconnect` |

---

## Verification

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --workspace
cargo nextest run --workspace
cargo check --all-targets --workspace
cargo bench --no-run
```

Key check: `test_server_accepts_new_client_after_disconnect` passes, proving sequential client reconnection works.
