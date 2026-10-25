# Changelog

All notable changes to this crate. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/); the
version is one number for the whole project, and while it is below 1.0 the minor number is the breaking one.
`ROADMAP.md` has the longer account of each item.

## [0.16.11]

### Changed

- Docs only: `README_HYBRID.md` and `HYBRID_IMPLEMENTATION_SUMMARY.md` no longer say the SDK was never run against a real server or that the server handler is missing; the unmeasured benchmark table is deleted.

## [0.16.10]

### Changed

- Test fix: `local_mode_with_the_write_trap_reports_trapped_writes` creates a 64 KiB service; with the default 8 KiB the trap has no whole page on macOS Apple Silicon (16 KiB pages) and `arm_trap` correctly returned `TooSmall`.
- CI only: the e2e suite also runs, as non-required `e2e-platforms` legs, on `ubuntu-24.04-arm`, `windows-latest`, `windows-11-arm`, `macos-latest` and `macos-15-intel`.

## [0.16.9]

### Changed

- `LICENSE` copyright holder and `Cargo.toml` `authors` are now Eric Evans (were "Commy Contributors"); the C FFI to-do is dropped from `HYBRID_IMPLEMENTATION_SUMMARY.md`. Docs and licence text only.

## [0.16.8]

### Changed

- Docs only: `ROADMAP.md` gains the rows a spec-to-roadmap audit found missing (stale hybrid docs, benchmark claims, NEON idea, C FFI, platform gaps) and its 0.15.0 section is marked built.

## [0.16.7]

### Removed

- The never-started background heartbeat task and its two private fields (`heartbeat_interval`, `heartbeat_task`). The server answers `Heartbeat` but evicts no idle session, and a background `recv()` would race request replies. `Client::heartbeat()` is unchanged.

## [0.16.6]

### Changed

- CI only: the `test` job also runs on `ubuntu-24.04-arm`, `windows-11-arm` and `macos-15-intel` (commy R2), so `PLATFORMS.md` can name those platforms TESTED from a green leg. The new legs are not required checks.
- CI only: the advisory CodeRipper `portfolio` job is repinned to `ee9c3950` (0.9.24).

## [0.16.5]

### Changed

- CI only: the e2e job builds commy 0.22.3 (`a79f20d9`; was 0.21.0), so the 14 e2e tests run against the current server.

## [0.16.4]

### Changed

- CI only: the advisory CodeRipper `portfolio` job is pinned to coderipper `d14a22b6` (0.9.14; was `30e012b7`).

## [0.16.3]

### Changed

- CodeRipper only: `.coderipper.toml` declares the six kept `reachability` items (`has_permission`, `is_admin`, `with_port`, `VERSION`, `into_inner`, `with_persistent`) as public API, each with its reason. No code change.

## [0.16.2]

### Changed

- CI only: the advisory CodeRipper `portfolio` job is pinned to coderipper `30e012b7` (was `a48fc6cc`).

## [0.16.1]

### Changed

- CI only: the advisory CodeRipper `portfolio` job is pinned to coderipper `a48fc6cc` (was `789b0122`).

## [0.16.0]

### Removed

- **Breaking** (items CodeRipper's `reachability` rule found with no caller; see the `cr-reachability` row in `ROADMAP.md`):
  `auth::validate_token_format`, `Connection::set_state`, `Connection::is_connected`, `watcher::create_temp_service_file`,
  `message::ServiceMetadata`, `message::Permission`, `LocalFileAccessor::as_slice`, `RemoteFileAccessor::update_buffer` and
  `RemoteFileAccessor::get_buffer`.

## [0.15.3]

### Added

- `LICENSE` (MIT, as declared in `Cargo.toml`) and this changelog.
- `SPDX-License-Identifier: MIT` header on every Rust source file.

### Changed

- `#[allow(dead_code)]` on the three heartbeat items now says why (`reason = ...`).

## [0.15.2]

### Changed

- CI: the e2e job builds commy 0.21.0 (`2f69f802`); an advisory CodeRipper `portfolio` job was added (not a required check).

## [0.15.1]

One release with 0.15.0, which was never published.

### Changed

- **Breaking:** `Client::authenticate` returns a `Session`. Services, tenant administration, grants, shared files, resizes
  and virtual files are methods on it, with the tenant implicit; calling one without authenticating does not compile.
  `Client::initialize` returns a `Session`. See `MIGRATION_GUIDE.md` for the 0.14 to 0.15.1 table.

## [0.15.0] (not published)

### Removed

- **Breaking:** `Client::read_variable`, `write_variable`, `subscribe`, `unsubscribe` and the `ReadVariable`,
  `WriteVariable`, `AllocateVariable`, `DeallocateVariable`, `Subscribe`, `Unsubscribe` client messages and
  `VariableData`, `VariableChanged` server messages. The commy server removed named variables (its Plan C6) and answers
  them with an unknown-message-type error.

## [0.14.2]

### Changed

- Docs only: the 0.15 design in `ROADMAP.md`.

## [0.14.1]

### Fixed

- `create_tenant`, `delete_tenant`, `read_variable`, `write_variable`, `subscribe` and `unsubscribe` refuse with
  `PermissionDenied`, before sending, when no tenant is authenticated (`heartbeat` is exempt, as on the server).

## [0.14.0]

### Removed

- **Breaking:** `SharedFile::generation` / `set_generation`; `complete_rotation` treats a commit whose `new_file_id` is the
  already-mapped file as a duplicate (commy-wire 0.5.0).

## [0.13.1]

### Changed

- CI hygiene: `.github/CODEOWNERS`, and `--locked` on every cargo call in CI.

## [0.13.0]

### Changed

- **Breaking:** an `AuthenticationResult` without a `request_id` is refused, so `authenticate` times out against servers
  older than commy 0.15.0 (commy-wire 0.4.0).

## [0.12.0]

### Changed

- **Breaking:** `ClientMessage::Authenticate` and `ServerMessage::AuthenticationResult` are the `commy-wire` 0.4.0 types
  (request id, protocol version, `method` plus credential fields).

## Earlier

0.11.x and before (shared-memory byte sharing S1-S8, rotation, grants, the e2e job, conformance vectors, platform
status) are recorded in the `S1-S8` and `audit-*` rows of `ROADMAP.md`.
