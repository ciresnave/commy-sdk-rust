//! A tenant session: the proof that a connection is authenticated to one tenant.
//!
//! [`Client::authenticate`] returns a [`Session`]. Every operation that needs an authenticated
//! connection (services, grants, shared files, resizes, tenant administration) is a method on
//! `Session`, so calling one without having authenticated does not compile. A session can still
//! outlive its authentication (the connection drops, the server revokes access); those calls fail
//! at run time with `ConnectionLost` or `PermissionDenied`.
//!
//! The operations are not callable on a [`Client`]:
//!
//! ```compile_fail,E0624
//! # async fn f() -> commy_sdk_rust::Result<()> {
//! let client = commy_sdk_rust::Client::new("wss://localhost:9000");
//! client.create_service("my_tenant", "svc", 4096).await?; // no such public method
//! # Ok(())
//! # }
//! ```
//!
//! and work once there is a session:
//!
//! ```no_run
//! # async fn f() -> commy_sdk_rust::Result<()> {
//! use commy_sdk_rust::{auth, Client};
//! let client = Client::new("wss://localhost:9000");
//! client.connect().await?;
//! let session = client.authenticate("my_tenant", auth::api_key("key".to_string())).await?;
//! session.create_service("svc", 4096).await?;
//! # Ok(())
//! # }
//! ```

use crate::auth::AuthContext;
use crate::client::{Client, RotationFill, RotationOutcome, SharedEvent};
use crate::error::Result;
use crate::local_map::NegotiatedFile;
use crate::service::Service;
use crate::virtual_file::VirtualVariableFile;
use std::sync::Arc;
use std::time::Duration;

/// A [`Client`] connection authenticated to one tenant.
///
/// Cheap to clone: clones share the connection and all state with the [`Client`] it came from.
/// Authenticating to a second tenant on the same client yields a second `Session`.
#[derive(Clone)]
pub struct Session {
    client: Client,
    auth: AuthContext,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("client_id", &self.client.id())
            .field("tenant_id", &self.auth.tenant_id)
            .finish()
    }
}

impl Session {
    pub(crate) fn new(client: Client, auth: AuthContext) -> Self {
        Self { client, auth }
    }

    /// The client this session runs on (connection state, heartbeat, disconnect).
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The tenant this session was authenticated to.
    pub fn tenant_id(&self) -> &str {
        &self.auth.tenant_id
    }

    /// What the server granted at authentication.
    pub fn auth_context(&self) -> &AuthContext {
        &self.auth
    }

    // ── services ─────────────────────────────────────────────────────────────

    /// Create a service of `size` bytes in this session's tenant; returns its id.
    pub async fn create_service(&self, service_name: &str, size: u64) -> Result<String> {
        self.client
            .create_service(&self.auth.tenant_id, service_name, size)
            .await
    }

    /// Get an existing service of this session's tenant (read-only, no side effects).
    pub async fn get_service(&self, service_name: &str) -> Result<Service> {
        self.client
            .get_service(&self.auth.tenant_id, service_name)
            .await
    }

    /// Delete a service of this session's tenant.
    pub async fn delete_service(&self, service_name: &str) -> Result<()> {
        self.client
            .delete_service(&self.auth.tenant_id, service_name)
            .await
    }

    // ── tenant administration (names the tenant explicitly: it may be another one) ──

    /// Create a new tenant (admin operation)
    ///
    /// Requires admin credentials or special permissions to create tenants.
    /// Returns the tenant ID on success.
    ///
    /// Returns error if:
    /// - Not connected to server
    /// - Tenant already exists
    /// - Insufficient permissions (need admin role)
    pub async fn create_tenant(&self, tenant_id: &str, tenant_name: &str) -> Result<String> {
        self.client.create_tenant(tenant_id, tenant_name).await
    }

    /// Delete a tenant (admin operation)
    ///
    /// Removes all services and data associated with the tenant.
    /// Requires admin credentials or special permissions.
    ///
    /// Returns error if:
    /// - Not connected to server
    /// - Tenant does not exist
    /// - Insufficient permissions (need admin role)
    /// - Tenant has active clients
    pub async fn delete_tenant(&self, tenant_id: &str) -> Result<()> {
        self.client.delete_tenant(tenant_id).await
    }

    // ── grants ───────────────────────────────────────────────────────────────

    /// Give `client_id` permissions (`tenant:read|write|admin`, `service:read|write|admin|resize`),
    /// tenant-wide or, with `service_id`, on that service. The caller needs admin rights there.
    pub async fn grant(
        &self,
        client_id: &str,
        service_id: Option<&str>,
        permissions: &[&str],
    ) -> Result<()> {
        self.client.grant(client_id, service_id, permissions).await
    }

    /// Take `permissions` from `client_id`. With `service_id` and no permissions, the client is
    /// removed from that service: it gets `Loss{REVOKED}` and the file rotates (see
    /// [`Session::complete_rotation`]).
    pub async fn revoke(
        &self,
        client_id: &str,
        service_id: Option<&str>,
        permissions: &[&str],
    ) -> Result<()> {
        self.client.revoke(client_id, service_id, permissions).await
    }

    /// List grants, optionally only those of one client and/or one service.
    pub async fn list_grants(
        &self,
        client_id: Option<&str>,
        service_id: Option<&str>,
    ) -> Result<Vec<commy_wire::GrantEntry>> {
        self.client.list_grants(client_id, service_id).await
    }

    /// Open (`true`) or close (`false`) a service to local memory mapping. Closed by default: until
    /// an administrator opens it, [`Session::open_shared`] gets no path and stays on WSS ranges.
    /// Needs `service:admin` on the service. Closing stops future offers; existing mappings end at
    /// the next rotation. Returns the state the server confirms.
    pub async fn set_local_mapping(&self, service_id: &str, enabled: bool) -> Result<bool> {
        self.client.set_local_mapping(service_id, enabled).await
    }

    // ── shared files ─────────────────────────────────────────────────────────

    /// Ask the server whether `service_id` may be mapped locally and, if it is offered and the
    /// nonce checks out, return the confirmed mapping. Any failure after the offer (unopenable
    /// file, bad nonce file, no ack) returns `local: None`: the caller stays on WSS ranges.
    ///
    /// While waiting, messages that do not carry this call's `request_id` are discarded.
    pub async fn negotiate_local(&self, service_id: &str) -> Result<NegotiatedFile> {
        self.client.negotiate_local(service_id).await
    }

    /// Map `service_id` locally when the server offers it (see [`Session::negotiate_local`]) and
    /// start tracking it in scan mode. Returns the file id when local mode is active, `None` when
    /// the service stays on WSS ranges.
    pub async fn open_shared(&self, service_id: &str) -> Result<Option<u32>> {
        self.client.open_shared(service_id).await
    }

    /// Remote mode: fetch the whole file with `Resync` and keep a private copy that range frames
    /// keep current. Use when [`Session::open_shared`] returns `None` (no local mapping offered).
    /// Returns the file id.
    pub async fn open_remote(&self, service_id: &str) -> Result<u32> {
        self.client.open_remote(service_id).await
    }

    /// Write into a locally mapped service. Nothing is sent until [`Session::flush_shared`].
    pub async fn shared_write(&self, service_id: &str, offset: u64, bytes: &[u8]) -> Result<()> {
        self.client.shared_write(service_id, offset, bytes).await
    }

    /// Copy `len` bytes at `offset` out of a mapped or remote-mirrored service.
    pub async fn shared_read(&self, service_id: &str, offset: u64, len: u64) -> Result<Vec<u8>> {
        self.client.shared_read(service_id, offset, len).await
    }

    /// Report what changed in a locally mapped service to the server, one `RangesChanged` per
    /// batch of up to 1024 ranges, each answered by a `FlushAck`. Returns the highest `seq` seen.
    pub async fn flush_shared(&self, service_id: &str) -> Result<u64> {
        self.client.flush_shared(service_id).await
    }

    /// Wait up to `wait` for one server message and, if it is a `RangesUpdated` or range frame for
    /// a mapped service, apply it so it is not reported as a local change. Returns `true` when an
    /// update was applied. Other messages are discarded; use [`Session::poll_shared_events`] to see
    /// rotation and loss events.
    pub async fn poll_shared_updates(&self, wait: Duration) -> Result<bool> {
        self.client.poll_shared_updates(wait).await
    }

    /// Like [`Session::poll_shared_updates`], but also reports rotation and loss events.
    pub async fn poll_shared_events(&self, wait: Duration) -> Result<SharedEvent> {
        self.client.poll_shared_events(wait).await
    }

    /// Trap writes to a locally mapped service instead of scanning every page at flush
    /// ([`SharedFile::arm_trap`](crate::shared_file::SharedFile::arm_trap)); `pool_pages / 2` pages
    /// are trapped. Opt-in.
    ///
    /// # Safety
    /// While armed, no thread may block `SIGSEGV`/`SIGBUS` while writing the mapping (Unix), and
    /// nothing may read from the OS into it.
    pub async unsafe fn arm_shared_trap(&self, service_id: &str, pool_pages: usize) -> Result<()> {
        unsafe { self.client.arm_shared_trap(service_id, pool_pages) }.await
    }

    // ── resize (rotation) ────────────────────────────────────────────────────

    /// Ask the server to replace a service's file. Returns the rotation id. A resize also needs
    /// the other SDKs to drain; this client then sees `RotationPending` / `RotationCommit` like the
    /// others.
    pub async fn request_rotation(
        &self,
        service_id: &str,
        reason: commy_wire::RotateReason,
        new_size: Option<u64>,
        revoke_client_id: Option<String>,
    ) -> Result<String> {
        self.client
            .request_rotation(service_id, reason, new_size, revoke_client_id)
            .await
    }

    /// Answer a resize `RotationPending`: refuse new writes, flush into the old file, send
    /// `RotationDrained`, then wait for the end of the rotation (at most `deadline_ms + fill_ms`
    /// plus a grace period). Returns `Committed` (switched to the new file), `Aborted` (writes
    /// resume on the old file), or, for the requester only, `Prepared`: fill the new file with
    /// [`Session::begin_fill`] and finish with [`Session::finish_fill`]. Writers get an error until the
    /// end; old-offset writes are never replayed into the new layout.
    pub async fn drain_for_rotation(
        &self,
        pending: &commy_wire::RotationPending,
    ) -> Result<RotationOutcome> {
        self.client.drain_for_rotation(pending).await
    }

    /// Requester: get the prepared file ready to fill. With a `path` (same machine) the file is
    /// mapped directly; without one the new bytes are buffered and sent as range frames by
    /// [`Session::finish_fill`]. Read the old layout with [`Session::shared_read`].
    pub fn begin_fill(
        &self,
        pending: &commy_wire::RotationPending,
        prepared: &commy_wire::RotationPrepared,
    ) -> Result<RotationFill> {
        self.client.begin_fill(pending, prepared)
    }

    /// Requester: the new file is filled. Send it (remote), say `RotationFilled`, and wait for the
    /// end of the rotation: `Committed` (switched) or `Aborted` (the old file stays).
    pub async fn finish_fill(&self, fill: RotationFill) -> Result<RotationOutcome> {
        self.client.finish_fill(fill).await
    }

    /// Requester: give up. The server keeps the old file and sends `RotationAborted` to everyone.
    pub async fn abort_fill(&self, fill: RotationFill) -> Result<RotationOutcome> {
        self.client.abort_fill(fill).await
    }

    /// Switch a service to the file named by a `RotationCommit`: re-run the local handshake (or
    /// `Resync` when no mapping is offered). After a revocation, unflushed local writes are
    /// written into the new file and flushed; after a resize they are never replayed. A commit
    /// that names the already-mapped file as its new file is a duplicate and ignored.
    pub async fn complete_rotation(&self, commit: &commy_wire::RotationCommit) -> Result<()> {
        self.client.complete_rotation(commit).await
    }

    // ── virtual files and file watching ──────────────────────────────────────

    /// Initialize file watcher for hybrid mode (public - for testing/special cases)
    pub async fn init_file_watcher(&self) -> Result<()> {
        self.client.init_file_watcher().await
    }

    /// Get a service of this session's tenant as a virtual variable file.
    pub async fn get_virtual_service_file(
        &self,
        service_name: &str,
    ) -> Result<Arc<VirtualVariableFile>> {
        self.client
            .get_virtual_service_file(&self.auth.tenant_id, service_name)
            .await
    }

    /// Start monitoring virtual files for changes (public - for testing/special cases)
    ///
    /// This spawns a background task that watches for file changes
    /// and automatically detects which variables have changed using SIMD
    pub async fn start_file_monitoring(&self) -> Result<()> {
        self.client.start_file_monitoring().await
    }

    /// Get next file change event (blocks until a file changes)
    pub async fn wait_for_file_change(&self) -> Result<Option<crate::watcher::FileChangeEvent>> {
        self.client.wait_for_file_change().await
    }

    /// Try to get next file change event (non-blocking)
    pub async fn try_get_file_change(&self) -> Result<Option<crate::watcher::FileChangeEvent>> {
        self.client.try_get_file_change().await
    }

    /// Stop file monitoring
    pub async fn stop_file_monitoring(&self) -> Result<()> {
        self.client.stop_file_monitoring().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ClientMessage, ServerMessage};

    /// A session on a mock connection whose next reply is `reply`.
    async fn session_with_reply(
        tenant: &str,
        reply: ServerMessage,
    ) -> (Session, tokio::sync::mpsc::UnboundedReceiver<ClientMessage>) {
        let client = Client::new("wss://test");
        let (conn, server_tx, client_rx) = crate::connection::Connection::new_for_test();
        server_tx.send(reply).expect("pre-send failed");
        client.inject_connection_for_test(conn).await;
        client.inject_auth_for_test(tenant).await;
        let session = Session::new(client, AuthContext::new(tenant.to_string(), vec![]));
        (session, client_rx)
    }

    #[tokio::test]
    async fn session_operations_use_the_sessions_tenant() {
        let (session, mut sent) = session_with_reply(
            "tenant_x",
            ServerMessage::Service {
                service_id: "svc-1".to_string(),
                service_name: "cfg".to_string(),
                tenant_id: "tenant_x".to_string(),
                file_path: None,
            },
        )
        .await;
        assert_eq!(session.tenant_id(), "tenant_x");

        let id = session.create_service("cfg", 4096).await.unwrap();
        assert_eq!(id, "svc-1");
        match sent.try_recv().expect("create_service must send") {
            ClientMessage::CreateService {
                tenant_id,
                service_name,
                size,
            } => {
                assert_eq!(tenant_id, "tenant_x");
                assert_eq!(service_name, "cfg");
                assert_eq!(size, 4096);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn clones_share_the_connection_and_tenant() {
        let (session, _sent) = session_with_reply(
            "tenant_x",
            ServerMessage::Result {
                request_id: String::new(),
                success: true,
                message: String::new(),
            },
        )
        .await;
        let copy = session.clone();
        assert_eq!(copy.tenant_id(), "tenant_x");
        assert_eq!(copy.client().id(), session.client().id());
        assert!(copy.client().is_authenticated_to("tenant_x").await);
    }
}
