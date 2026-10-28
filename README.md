# commy-sdk-rust

Rust client for [Commy](https://github.com/ciresnave/commy), a server that coordinates byte ranges of shared files between processes.
A client connects over WSS, authenticates to a **tenant**, and gets a **service**: a fixed-size file of bytes. Clients on the server's
machine map that file directly; clients elsewhere keep a private mirror that stays in sync through range messages.

**Status: pre-1.0.** The first coordinated release with Commy (which freezes the `commy-wire` message layout) has not happened, so any
minor version may break the API (see [CHANGELOG.md](CHANGELOG.md)). What has been run, and on which platform, is in
[PLATFORMS.md](PLATFORMS.md); the open work is in [ROADMAP.md](ROADMAP.md).

## Install

```toml
[dependencies]
commy-sdk-rust = "0.17"
tokio = { version = "1", features = ["full"] }
```

The library is `commy_sdk_rust`. The minimum supported Rust version is in `Cargo.toml` (`rust-version`).

## Connect, authenticate, create a service

`Client::authenticate` returns a `Session`: the proof that this connection is authenticated to one tenant. Everything that needs a
login is a method on `Session`, so calling it without having authenticated does not compile.

```rust,no_run
use commy_sdk_rust::{auth, Client};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new("wss://localhost:9000");
    client.connect().await?;
    let session = client
        .authenticate("my_tenant", auth::api_key("api_key_value".to_string()))
        .await?;

    let service_id = session.create_service("counters", 65_536).await?;
    println!("created {service_id}");
    Ok(())
}
```

## Share bytes

Open the service locally when the server offers a mapping, otherwise as a remote mirror; write, flush, and apply what others flushed.

```rust,no_run
# use std::time::Duration;
# async fn f(session: commy_sdk_rust::Session, service_id: String) -> commy_sdk_rust::Result<()> {
if session.open_shared(&service_id).await?.is_none() {
    session.open_remote(&service_id).await?; // no local mapping offered: WSS ranges only
}
session.shared_write(&service_id, 8, b"hello").await?; // nothing is sent yet
session.flush_shared(&service_id).await?; // reports the changed ranges to the server

// Other members' writes arrive as range updates; applying them keeps them from echoing back.
if session.poll_shared_updates(Duration::from_secs(1)).await? {
    let bytes = session.shared_read(&service_id, 8, 5).await?;
    println!("{bytes:?}");
}
# Ok(())
# }
```

A service is closed to local mapping until an administrator opens it with `Session::set_local_mapping(&service_id, true)`.
Resizing a service and removing a member's access replace the file ("rotation"); [ARCHITECTURE.md](ARCHITECTURE.md) explains how
the SDK drains, fills and switches, and [API.md](API.md) lists every `Session` method.

## Where to read next

| file | what it is |
|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | local mapping, remote mirror, change detection, rotation |
| [API.md](API.md) | the `Session` and `Client` methods, errors and permissions |
| [MIGRATION_GUIDE.md](MIGRATION_GUIDE.md) | moving from 0.14 and earlier |
| [PLATFORMS.md](PLATFORMS.md) | which OS and architecture each claim was run on |
| [examples/README.md](examples/README.md) | the runnable examples |
| [CHANGELOG.md](CHANGELOG.md) | what changed in each version |

## Tests

`cargo test` runs the unit and integration tests, including the `commy-wire` golden vectors
(`tests/wire_vectors.rs`). `tests/e2e.rs` (feature `e2e`, `COMMY_BIN` naming a built `commy` binary) runs the SDK against the real
server; CI does this on six platforms.

## Licence

MIT, copyright Eric Evans. See [LICENSE](LICENSE).
