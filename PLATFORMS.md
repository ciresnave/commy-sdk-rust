# Platform status

What has actually been run, and where. **TESTED** means a test or build ran on that platform and the evidence is
named; **UNVERIFIED** means nobody has run it (no assumption is made either way). Updated 2026-10-18 at 0.2.1.

## Library and unit/integration tests (`cargo test --all-features`)

| platform | status | evidence |
|---|---|---|
| Linux x86_64 glibc (ubuntu-latest) | TESTED | CI `test (ubuntu-latest)` on every PR |
| Windows x86_64 MSVC (windows-latest) | TESTED | CI `test (windows-latest)`; also the lane's local runs |
| macOS Apple Silicon (macos-latest) | TESTED | CI `test (macos-latest)` |
| Linux x86_64 glibc, Docker rust:bookworm | TESTED | local run before each PR (not CI) |
| MSRV 1.94 | TESTED (compile only) | CI `MSRV` job: `cargo check --all-targets --all-features` on ubuntu |
| Linux musl, aarch64 Linux, Windows arm64, macOS Intel, FreeBSD and other BSDs, 32-bit targets | UNVERIFIED | no job, no local run |

CI also runs `cargo fmt --check`, `clippy -D warnings`, `cargo doc` (warnings denied) and `cargo deny`
(advisories, licences, bans, sources) on ubuntu. Those are platform-independent.

## What the tests do **not** exercise

* **Nothing runs against a real Commy server.** The five integration tests that do are `#[ignore]` (see `ROADMAP.md`).
  So TLS (`rustls-tls-native-roots`), the real WebSocket handshake and the real wire format are UNVERIFIED on every OS.
  Evidence that this matters: the real server's error codes (`"NotFound"`, `"InternalError"`, ...) do not parse in the SDK
  (audit finding 4, reproduced by test on 0112457) and the SDK would time out instead of reporting them.
* **Memory mapping and the file watcher** (`file_accessor`, `watcher`, `virtual_file`) are covered by unit tests only
  (CI on the three OSes above). Behaviour with real concurrent writers, `notify` backends (inotify, FSEvents,
  ReadDirectoryChangesW) and network filesystems is UNVERIFIED. The planned page-fault write trap has its own
  conformance matrix in the `dirty-ranges` repository (spike/trap-conformance).
* **Unix-only code paths** were broken until CI first ran on Linux/macOS: `watcher::create_temp_service_file` returned
  NotFound on every Unix (fixed in 0.1.2). Windows-only local runs do not cover such paths; run Linux (Docker) as well.

## How to reproduce

```
cargo test --all-features --no-fail-fast
docker run --rm -v "$PWD:/src:ro" -w /src -e CARGO_TARGET_DIR=/tmp/t rust:bookworm cargo test --all-features --no-fail-fast
cargo +1.94.0 check --all-targets --all-features
```
