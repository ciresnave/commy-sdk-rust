# Migration Guide

The crate is pre-1.0: the minor number is the breaking one. Each section below is one breaking step; apply them in order.
[CHANGELOG.md](CHANGELOG.md) has the full list of what changed in each version.

## 0.16 to 0.17.0 (breaking)

The virtual-file layer is removed. It never reached the server (`commy-wire` has no such messages, and the Commy server answers
them with an "unknown message type" error), so a program that used it was not sharing bytes with anyone.

| 0.16 | 0.17.0 |
|---|---|
| `session.get_virtual_service_file(name)` and `VirtualVariableFile` | `session.get_service(name)` for the id, then `open_shared` / `open_remote`, `shared_write`, `shared_read` |
| `session.init_file_watcher()`, `start_file_monitoring()`, `stop_file_monitoring()` | nothing: `flush_shared` finds changes itself (scan by default, `arm_shared_trap` to trap writes) |
| `session.wait_for_file_change()` / `try_get_file_change()` and `FileChangeEvent` | `session.poll_shared_updates(wait)` / `poll_shared_events(wait)` for other members' changes |
| `FileAccessor`, `LocalFileAccessor`, `RemoteFileAccessor` | `open_shared` chooses local mapping or the remote mirror; `open_remote` forces the mirror |
| `ClientMessage::ReportVariableChanges`, `ServerMessage::VariableChangesAcknowledged` | `flush_shared` (`RangesChanged` / `FlushAck` from `commy-wire`) |
| `CommyError::WatcherError`, `VariableNotFound`, `SimdError` | no longer produced; remove the match arms |
| `Client::initialize` started a file watcher | it only connects and authenticates |

The dependencies `notify`, `dirs` and `async-trait` are gone with it. `examples/hybrid_client.rs` is removed; see
[examples/README.md](examples/README.md) for what remains.

## 0.15 to 0.16.0 (breaking)

Nine public items that nothing called were deleted: `auth::validate_token_format`, `Connection::set_state`,
`Connection::is_connected`, `watcher::create_temp_service_file`, `message::ServiceMetadata`, `message::Permission`,
`LocalFileAccessor::as_slice`, `RemoteFileAccessor::update_buffer` and `RemoteFileAccessor::get_buffer`. A program that used one
of them has to stop; there is no replacement because nothing in the SDK used them.

## 0.14 to 0.15.1 (breaking)

0.15.0 was never published; 0.15.1 is the one release. Two breaking changes:

1. **Named variables are gone.** `Client::read_variable`, `write_variable`, `subscribe`, `unsubscribe` and their messages
   (`ReadVariable`, `WriteVariable`, `AllocateVariable`, `DeallocateVariable`, `Subscribe`, `Unsubscribe`, `VariableData`,
   `VariableChanged`) were removed: the Commy server dropped them (its Plan C6) and answers them with an "unknown message type"
   error. Use the shared-file calls (`open_shared`, `shared_write`, `shared_read`, `flush_shared`, `open_remote`). What the bytes
   mean is now up to the program: a service is a file, not a set of named variables.
2. **`authenticate` returns a `Session`.** Every operation that needs a login is a method on `Session`, so calling one
   without authenticating does not compile. The tenant is the session's, so it is no longer an argument.

| 0.14 | 0.15.1 |
|---|---|
| `client.authenticate(t, creds).await?;` | `let session = client.authenticate(t, creds).await?;` |
| `client.create_service(t, name, size)` | `session.create_service(name, size)` |
| `client.get_service(t, name)` / `delete_service(t, name)` | `session.get_service(name)` / `session.delete_service(name)` |
| `client.open_shared(id)`, `shared_write`, `grant`, `request_rotation`, ... | the same call on `session` |
| `client.create_tenant(id, name)` / `delete_tenant(id)` | `session.create_tenant(id, name)` / `session.delete_tenant(id)` |
| `Client::initialize(url, t, creds)` returns `Client` | returns a `Session`; `session.client()` is the client |
| `client.heartbeat()`, `disconnect()`, `is_connected()`, `connection_state()` | unchanged (on `Client`; `session.client()`) |

A `Session` is `Clone` and shares the connection with its `Client`. It can outlive its authentication (connection lost, access
revoked); those calls fail at run time with `ConnectionLost` or `PermissionDenied`.
