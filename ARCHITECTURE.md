# Architecture

How the SDK keeps a file of bytes in sync between processes. Every statement here is about code in `src/` at the version in
`Cargo.toml`; the wire messages are the `commy-wire` crate's, not this crate's.

## Pieces

| piece | file | job |
|---|---|---|
| `Client` | `src/client.rs`, `connection.rs` | one WSS connection; which tenants it is authenticated to; request/reply matching by `request_id` |
| `Session` | `src/session.rs` | a tenant-bound handle returned by `Client::authenticate`; every operation that needs a login |
| `LocalMap` | `src/local_map.rs` | maps the service file the server names and proves the client shares the server's machine |
| `SharedFile` | `src/shared_file.rs` | the bytes of one service on this client, and the record of what changed |
| `dirty-ranges` | separate crate | finds the changed ranges: a scan against a baseline, or an opt-in write trap |

## Two ways to hold the bytes

`Session::open_shared` tries local mapping and returns `Some(file_id)` when it works, `None` when the service stays on WSS ranges;
`Session::open_remote` is the other mode.

**Local mapping.** The server may offer a path to the service file. The SDK opens it, reads a nonce from a `0600` side file in the
tenant directory (never from the service file), and echoes the nonce to the server over the authenticated connection
(`ConfirmLocal`, answered by `ConfirmLocalAck`). An address match alone is not proof of a shared filesystem; the nonce is. Any
failure after the offer (file will not open, wrong nonce, no ack) leaves the client on WSS ranges, not in an error. A service is
closed to local mapping until an administrator calls `Session::set_local_mapping(service_id, true)`.

**Remote mirror.** `open_remote` fetches the whole file with `Resync` and keeps a private copy that range frames keep current.

In both modes the program reads with `shared_read` and writes with `shared_write`. Nothing is sent by a write; `flush_shared` sends.

## Change detection

`flush_shared` needs to know which bytes changed.

- **Scan (the default, every platform).** The file is diffed against a baseline kept by `dirty_ranges::Scanner`. It needs no
  operating-system support.
- **Write trap (opt-in).** `Session::arm_shared_trap` (an `unsafe` fn) write-protects the pages so a write is noticed when it happens,
  not found by a scan. While armed, no thread may block `SIGSEGV`/`SIGBUS` while writing the mapping (Unix), and nothing may read
  from the operating system straight into it. On macOS a write from a thread that blocks those signals hangs rather than dying. The
  mapping must contain at least one whole OS page (16 KiB on Apple Silicon), or arming fails with `TooSmall`. The per-platform
  evidence is in [PLATFORMS.md](PLATFORMS.md).

## Flush and inbound updates

`flush_shared` sends the changed ranges as `RangesChanged`, at most 1024 ranges per message, and each message is answered by a
`FlushAck`; it returns the highest `seq` seen. Other members' writes arrive as `RangesUpdated`. `poll_shared_updates` applies one:
through a second, SDK-only view of the same file and the baseline in one step, so what came from the server is never reported back
as a local change. A frame whose `seq` is lower than the last applied for that file is dropped as stale. `poll_shared_events` also
reports rotation and loss events.

## Rotation: replacing the file

The server replaces a service's file when it is resized (`Session::request_rotation`) or when a member's access is revoked.

1. Everyone gets `RotationPending` and runs `drain_for_rotation`: new writes are refused, pending writes are flushed into the old
   file, `RotationDrained` is sent, and the call waits for the end of the rotation.
2. The outcome is `Committed` (switch to the new file), `Aborted` (writes resume on the old file) or, for the requester only,
   `Prepared`. The requester then fills the new file with `begin_fill` and `finish_fill` (or gives up with `abort_fill`).
3. `complete_rotation` switches to the committed file by re-running the local handshake, or `Resync` when no mapping is offered.

Writes made while a rotation drains get an error; after a resize, writes made against the old offsets are never replayed into the new
layout. After a revocation, a revoked client's unflushed local writes are written into the new file and flushed.

## Authentication and errors

Authentication is per tenant and additive: one connection can authenticate to several tenants, and a tenant can be lost. That is why
the typed handle is a tenant-bound `Session` and not a `Client` with a state parameter. A `Session` can outlive its authentication (the
connection drops, the server revokes access); those calls return `CommyError::ConnectionLost` or `PermissionDenied` at run time.
`CommyError` lists the failure kinds.

## What is not here

There are no named variables: the Commy server removed them, and this crate's variable calls went with them in 0.15.0. A layer of
"virtual variable files" with a file watcher, which never reached the server, was removed in 0.17.0 (see [CHANGELOG.md](CHANGELOG.md)).
Layout and meaning of the bytes in a service belong to the program that uses it.
