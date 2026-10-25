# Examples

Two examples, both self-contained: `CommyServer` (`src/examples_support.rs`) downloads a Commy server binary, generates a
self-signed certificate, starts the server, and stops it at the end.

| example | run with | what it shows |
|---|---|---|
| `basic_client` | `cargo run --example basic_client` | connect, authenticate, create / get / delete a service, heartbeat, disconnect |
| `permissions_example` | `cargo run --example permissions_example` | how administrator, read-only and creator clients are allowed or refused each service operation |

The examples are not run in CI (they fetch a binary from the network). The tested path is `tests/e2e.rs`, which runs the SDK
against a `commy` binary built from a pinned commit; see [PLATFORMS.md](../PLATFORMS.md). For the shared-bytes calls (`open_shared`,
`shared_write`, `flush_shared`, ...) read the end-to-end tests or [README.md](../README.md).

There is also a demo GUI, `cargo run --bin examples_gui`, that launches the examples from a browser page.
