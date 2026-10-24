# Platform status

What has actually been run, and where. **TESTED** means a test or build ran on that platform and the evidence is
named; **UNVERIFIED** means nobody has run it (no assumption is made either way). Updated 2026-10-20 at 0.11.5
(commy R1 gate item 5: every OS and architecture named as TESTED or UNVERIFIED).

## Library and unit/integration tests (`cargo test --features logging`)

| platform | status | evidence |
|---|---|---|
| Linux x86_64 glibc (ubuntu-latest) | TESTED | CI `test (ubuntu-latest)` on every PR |
| Windows x86_64 MSVC (windows-latest) | TESTED | CI `test (windows-latest)`; also the lane's local runs |
| macOS Apple Silicon (macos-latest) | TESTED | CI `test (macos-latest)` |
| Linux x86_64 glibc, Docker rust:bookworm | TESTED | local run before each PR (not CI) |
| MSRV 1.94 | TESTED (compile only) | CI `MSRV` job: `cargo check --all-targets --all-features` on ubuntu |
| Linux aarch64 glibc (ubuntu-24.04-arm) | TESTED | CI `test (ubuntu-24.04-arm)`, CI run 40128032360 on PR #47 |
| Windows arm64 (windows-11-arm) | TESTED | CI `test (windows-11-arm)`, same run |
| macOS Intel (macos-15-intel) | TESTED | CI `test (macos-15-intel)`, same run |
| Linux musl, aarch64 musl, FreeBSD and other BSDs, 32-bit targets | UNVERIFIED | no job, no local run |

CI also runs `cargo fmt --check`, `clippy -D warnings`, `cargo doc` (warnings denied) and `cargo deny`
(advisories, licences, bans, sources) on ubuntu. Those are platform-independent.

## Against a real Commy server (`tests/e2e.rs`, feature `e2e`)

The CI job `e2e (real commy server)` (a required check) builds `ciresnave/commy` at a pinned commit and runs the SDK
against the binary over real TLS with a generated self-signed certificate. It covers: connect and authenticate, the
service lifecycle, grants and denial, remote-mode range exchange, local mapping (opened with `set_local_mapping`, the nonce
handshake, a mapping shared by two local clients and a remote one), the write trap, a resize filled through the prepared
file (remote and local), an abort, a straggler, a revocation, and a server restart that keeps services and grants.

| platform | status | evidence |
|---|---|---|
| Linux x86_64 glibc (ubuntu-latest) | TESTED | CI `e2e (real commy server)` |
| Linux x86_64 glibc, Docker rust:bookworm | TESTED | local run before each PR |
| Windows x86_64 MSVC (windows-latest) | TESTED | CI `e2e (windows-latest)`, CI run 40139937817 on PR #51 (head 78b2df0); also the lane's local runs |
| Linux aarch64 glibc (ubuntu-24.04-arm) | TESTED | CI `e2e (ubuntu-24.04-arm)`, same run |
| Windows arm64 (windows-11-arm) | TESTED | CI `e2e (windows-11-arm)`, same run |
| macOS Apple Silicon (macos-latest) | TESTED | CI `e2e (macos-latest)`, same run; needed a 64 KiB trap-test service (16 KiB pages) |
| macOS Intel (macos-15-intel) | TESTED | CI `e2e (macos-15-intel)`, same run |
| every other platform | UNVERIFIED | no job, no local run |

Only the commy server built from the pinned commit is exercised; what it does not implement yet (real
authentication, a durable audit log) is not tested here.

## Write detection (the change tracking behind `flush_shared`)

* **Scan mode is the default everywhere**: `SharedFile::commit` diffs the file against a baseline. It needs no OS
  support, so it is as TESTED as the library tests above.
* **The write trap is opt-in** (`SharedFile::arm_trap`, `Client::arm_shared_trap`, both `unsafe`). Trapping is
  therefore never the default on any platform; the table says where it has run.

| platform | trap status | evidence |
|---|---|---|
| Linux x86_64 glibc | TESTED | lib tests in CI `test (ubuntu-latest)`; local-mode e2e with the trap in CI `e2e` |
| Windows x86_64 | TESTED | lib tests in CI `test (windows-latest)`; local-mode e2e with the trap run locally |
| macOS Apple Silicon | TESTED | CI `test (macos-latest)` and `e2e (macos-latest)` with the trap, CI run 40139937817 on PR #51 (head 78b2df0) |
| Linux aarch64 glibc; Windows arm64; macOS Intel | TESTED | CI `test` and `e2e` legs on those runners, same run (the e2e write-trap test passes on all of them) |
| Linux aarch64 musl | UNVERIFIED in the SDK | `dirty-ranges` lists it UNVERIFIED too; this crate has not run on it |
| NetBSD, OpenBSD, Linux with 64 KiB pages, 32-bit targets | UNVERIFIED | no job, no local run |

Limits that come with the trap (from `dirty-ranges`; its README has the per-OS conformance table):

* A thread that blocks `SIGSEGV`/`SIGBUS` must not write a trapped mapping. On Linux the process is killed (loud, not silent);
  **on macOS the write HANGS instead** (measured by `dirty-ranges`, not by this crate: 5 of 5 rounds on macos-latest and 5 of 5 on macos-15-intel, its CI run 40137219741). This crate has no test of it; call `dirty_ranges::fault_signals_blocked()` on the writing thread first.
* Nothing may read from the operating system straight into a trapped mapping (`read`, `recv`): the kernel reports `EFAULT`.
* `vm.max_map_count` exhaustion and coexistence with other signal handlers (JVM, Go, sanitizers) are UNVERIFIED.

## What the tests do **not** exercise

* **Behaviour with real concurrent writers across processes** beyond the two-or-three-client scenarios in `tests/e2e.rs`,
  **network filesystems**, and the **file watcher backends** (inotify, FSEvents, ReadDirectoryChangesW) are UNVERIFIED.
  `file_accessor`, `watcher` and `virtual_file` are covered by unit tests only (CI on the three OSes above).
* **The real wire against any server but Commy's own** is not exercised; the golden vectors of `commy-wire`
  (`tests/wire_vectors.rs`) run against the SDK's message types, with the documented exceptions listed in that file.
* **Unix-only code paths** were once broken until CI first ran on Linux/macOS (a temp-file helper in `watcher`, fixed in 0.1.2 and removed in 0.16.0). A Windows-only local run does not cover such paths; run Linux (Docker) as well.

## How to reproduce

```
cargo test --features logging --no-fail-fast
docker run --rm -v "$PWD:/src:ro" -w /src -e CARGO_TARGET_DIR=/tmp/t rust:bookworm cargo test --lib
cargo +1.94.0 check --all-targets --all-features
# against a real server (COMMY_BIN names a built commy binary; see the CI job for how it is built):
COMMY_BIN=/path/to/commy cargo test --features e2e --test e2e -- --test-threads=1
```
