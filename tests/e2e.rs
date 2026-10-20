//! End-to-end: the SDK against a real Commy server over WSS with a real self-signed certificate.
//!
//! Built only with `--features e2e`; `COMMY_BIN` must name a built `commy` binary (CI builds one from
//! ciresnave/commy). The server runs in a temporary directory, on a free loopback port, with
//! development authentication on (the only way to authenticate today).

use commy_sdk_rust::client::{RotationOutcome, SharedEvent};
use commy_sdk_rust::{auth, Client};
use commy_wire::RotateReason;
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

struct Server {
    child: Child,
    bin: String,
    port: u16,
    extra_env: Vec<(String, String)>,
    _dir: tempfile::TempDir,
    url: String,
    cert_pem: String,
}

/// Start the server binary in `dir` (its working directory, so its `commy_state` and tenant
/// directories live there) on `port`, appending to the same logs.
fn spawn_server(
    bin: &str,
    dir: &std::path::Path,
    port: u16,
    extra_env: &[(String, String)],
) -> Child {
    let log = |n: &str| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(n))
            .unwrap()
    };
    Command::new(bin)
        .current_dir(dir)
        .env("COMMY_LISTEN_ADDR", "127.0.0.1")
        .env("COMMY_LISTEN_PORT", port.to_string())
        .env("COMMY_TLS_CERT_PATH", dir.join("cert.pem"))
        .env("COMMY_TLS_KEY_PATH", dir.join("key.pem"))
        .env("COMMY_ALLOW_DEV_AUTH", "1")
        // Anything not starting with postgresql:// or mysql:// selects in-memory auth storage.
        .env("DATABASE_URL", "memory")
        .envs(extra_env.iter().map(|(k, v)| (k, v)))
        .stdout(log("out.log"))
        .stderr(log("err.log"))
        .stdin(Stdio::null())
        .spawn()
        .expect("start the commy server")
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    async fn start() -> Self {
        Self::start_with(&[]).await
    }

    /// Start with extra environment variables for the server (limits and timeouts).
    async fn start_with(extra_env: &[(&str, &str)]) -> Self {
        let extra_env: Vec<(String, String)> = extra_env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let bin = std::env::var("COMMY_BIN")
            .expect("COMMY_BIN must name a built commy server binary for the e2e tests");
        let dir = tempfile::tempdir().unwrap();
        let cert =
            rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string(), "localhost".into()])
                .unwrap();
        let cert_pem = cert.cert.pem();
        std::fs::write(dir.path().join("cert.pem"), &cert_pem).unwrap();
        std::fs::write(dir.path().join("key.pem"), cert.signing_key.serialize_pem()).unwrap();
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let child = spawn_server(&bin, dir.path(), port, &extra_env);
        let mut server = Server {
            child,
            bin,
            port,
            extra_env,
            _dir: dir,
            url: format!("wss://127.0.0.1:{port}"),
            cert_pem,
        };
        server.wait_ready().await;
        server
    }

    /// Ready when a client can connect.
    async fn wait_ready(&mut self) {
        for _ in 0..100 {
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!("server exited early ({status}): {}", self.logs());
            }
            let probe = self.client();
            if probe.connect().await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("server never accepted a connection: {}", self.logs());
    }

    /// Kill the server and start it again on the same port and state directory. Sessions, nonces and
    /// rotations in flight are gone; the service registry and the grants are what must survive.
    async fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.child = spawn_server(&self.bin, self._dir.path(), self.port, &self.extra_env);
        self.wait_ready().await;
    }

    fn logs(&self) -> String {
        let read = |n: &str| std::fs::read_to_string(self._dir.path().join(n)).unwrap_or_default();
        format!("stdout:\n{}\nstderr:\n{}", read("out.log"), read("err.log"))
    }

    fn client(&self) -> Client {
        Client::new(self.url.clone()).with_tls_root_pem(self.cert_pem.clone())
    }

    /// A connected, authenticated client on `tenant`.
    async fn session(&self, tenant: &str) -> Client {
        let c = self.client();
        c.connect().await.expect("connect");
        c.authenticate(tenant, auth::api_key("dev".to_string()))
            .await
            .expect("authenticate");
        c
    }
}

const WAIT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn client_without_the_root_certificate_is_refused() {
    let s = Server::start().await;
    let untrusting = Client::new(s.url.clone());
    assert!(
        untrusting.connect().await.is_err(),
        "a self-signed certificate must not be trusted by default"
    );
}

// Local mode (a shared mapping) needs the server to trust the client as local, which the binary cannot
// yet be told to do (commy C10); only remote mode runs against the binary.

#[tokio::test]
async fn remote_mode_two_clients_exchange_ranges() {
    let s = Server::start().await;
    let a = s.session("t_remote").await;
    let b = s.session("t_remote").await;
    let svc = a.create_service("t_remote", "svc", 65536).await.unwrap();
    let fa = a.open_remote(&svc).await.unwrap();
    let fb = b.open_remote(&svc).await.unwrap();
    assert_eq!(fa, fb, "one service, one file id");

    a.shared_write(&svc, 8, b"remote!").await.unwrap();
    a.flush_shared(&svc).await.unwrap();
    assert!(b.poll_shared_updates(WAIT).await.unwrap(), "B must hear A");
    assert_eq!(b.shared_read(&svc, 8, 7).await.unwrap(), b"remote!");

    b.shared_write(&svc, 64, b"back").await.unwrap();
    b.flush_shared(&svc).await.unwrap();
    assert!(a.poll_shared_updates(WAIT).await.unwrap(), "A must hear B");
    assert_eq!(a.shared_read(&svc, 64, 4).await.unwrap(), b"back");
    // What A applied from B must not echo back as A's own change.
    assert!(matches!(
        b.poll_shared_events(Duration::from_millis(500))
            .await
            .unwrap(),
        SharedEvent::None
    ));
}

/// Poll until the drain barrier opens for this client.
async fn next_pending(c: &Client) -> commy_wire::RotationPending {
    for _ in 0..20 {
        if let SharedEvent::RotationPending(p) = c.poll_shared_events(WAIT).await.unwrap() {
            return p;
        }
    }
    panic!("no RotationPending arrived");
}

/// Two remote clients on one service holding `b"old layout"` at offset 10.
async fn rotation_rig(s: &Server, tenant: &str) -> (Arc<Client>, Arc<Client>, String) {
    let a = Arc::new(s.session(tenant).await);
    let b = Arc::new(s.session(tenant).await);
    let svc = a.create_service(tenant, "svc", 8192).await.unwrap();
    a.open_remote(&svc).await.unwrap();
    b.open_remote(&svc).await.unwrap();
    a.shared_write(&svc, 10, b"old layout").await.unwrap();
    a.flush_shared(&svc).await.unwrap();
    assert!(b.poll_shared_updates(WAIT).await.unwrap());
    (a, b, svc)
}

#[tokio::test]
async fn resize_rotation_requester_fills_the_new_file_and_everyone_switches() {
    let s = Server::start().await;
    let (a, b, svc) = rotation_rig(&s, "t_resize").await;
    a.request_rotation(&svc, RotateReason::Resize, Some(16384), None)
        .await
        .unwrap();

    let requester = async {
        let p = next_pending(&a).await;
        let RotationOutcome::Prepared(r) = a.drain_for_rotation(&p).await.unwrap() else {
            panic!("the requester must be asked to fill");
        };
        let mut fill = a.begin_fill(&p, &r).unwrap();
        assert_eq!(fill.size(), 16384);
        // The service migrates its own layout: old offset 10 moves to 4096.
        let old = a.shared_read(&svc, 10, 10).await.unwrap();
        fill.write(4096, &old).unwrap();
        a.finish_fill(fill).await.unwrap()
    };
    let member = async {
        let p = next_pending(&b).await;
        b.drain_for_rotation(&p).await.unwrap()
    };
    let (ra, rb) = tokio::join!(requester, member);
    let (RotationOutcome::Committed(ca), RotationOutcome::Committed(cb)) = (ra, rb) else {
        panic!("both must commit");
    };
    assert_eq!((ca.generation, ca.size), (1, 16384));
    assert_eq!(ca.new_file_id, cb.new_file_id);
    assert_ne!(ca.new_file_id, ca.old_file_id);
    for c in [&a, &b] {
        assert_eq!(c.shared_read(&svc, 4096, 10).await.unwrap(), b"old layout");
        assert!(c.shared_read(&svc, 16380, 4).await.is_ok(), "new size");
        assert!(c.shared_read(&svc, 16384, 1).await.is_err());
        // Old-offset bytes are not replayed into the new layout.
        assert_eq!(c.shared_read(&svc, 10, 10).await.unwrap(), vec![0u8; 10]);
    }
    // Writers resume on the new file.
    b.shared_write(&svc, 12000, b"after").await.unwrap();
    b.flush_shared(&svc).await.unwrap();
    assert!(a.poll_shared_updates(WAIT).await.unwrap());
    assert_eq!(a.shared_read(&svc, 12000, 5).await.unwrap(), b"after");
}

#[tokio::test]
async fn resize_rotation_abort_keeps_the_old_file_and_writers_resume() {
    let s = Server::start().await;
    let (a, b, svc) = rotation_rig(&s, "t_abort").await;
    a.request_rotation(&svc, RotateReason::Resize, Some(16384), None)
        .await
        .unwrap();

    let requester = async {
        let p = next_pending(&a).await;
        let RotationOutcome::Prepared(r) = a.drain_for_rotation(&p).await.unwrap() else {
            panic!("the requester must be asked to fill");
        };
        let fill = a.begin_fill(&p, &r).unwrap();
        a.abort_fill(fill).await.unwrap()
    };
    let member = async {
        let p = next_pending(&b).await;
        b.drain_for_rotation(&p).await.unwrap()
    };
    let (ra, rb) = tokio::join!(requester, member);
    assert!(matches!(ra, RotationOutcome::Aborted(_)));
    assert!(matches!(rb, RotationOutcome::Aborted(_)));
    // Still the old file, nothing lost, writes allowed again.
    assert_eq!(a.shared_read(&svc, 10, 10).await.unwrap(), b"old layout");
    assert!(b.shared_read(&svc, 8192, 1).await.is_err(), "old size");
    b.shared_write(&svc, 100, b"resumed").await.unwrap();
    b.flush_shared(&svc).await.unwrap();
    assert!(a.poll_shared_updates(WAIT).await.unwrap());
    assert_eq!(a.shared_read(&svc, 100, 7).await.unwrap(), b"resumed");
}

#[tokio::test]
async fn connect_authenticate_and_disconnect() {
    let s = Server::start().await;
    let c = s.client();
    c.connect().await.expect("connect");
    let ctx = c
        .authenticate("t_auth", auth::api_key("demo_key".to_string()))
        .await
        .expect("dev authentication accepts any credential");
    assert_eq!(ctx.tenant_id, "t_auth");
    assert_eq!(c.authenticated_tenants().await, vec!["t_auth".to_string()]);
    c.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn service_lifecycle_and_heartbeat() {
    let s = Server::start().await;
    let c = s.session("t_life").await;
    let id = c.create_service("t_life", "config", 4096).await.unwrap();
    let svc = c.get_service("t_life", "config").await.unwrap();
    assert_eq!(svc.id, id);
    assert!(
        matches!(
            c.get_service("t_life", "missing").await,
            Err(commy_sdk_rust::CommyError::NotFound(_))
        ),
        "get_service must not create, and the legacy handler's CamelCase code must read as NotFound"
    );
    c.heartbeat().await.unwrap();
    c.disconnect().await.unwrap();
}

#[tokio::test]
async fn several_clients_work_at_once() {
    let s = Server::start().await;
    let mut tasks = Vec::new();
    for i in 0..3 {
        let c = s.client();
        tasks.push(tokio::spawn(async move {
            c.connect().await.expect("connect");
            c.authenticate(
                format!("t_multi_{i}"),
                auth::api_key("demo_key".to_string()),
            )
            .await
            .expect("authenticate");
            c.create_service(&format!("t_multi_{i}"), "svc", 4096)
                .await
                .expect("create_service");
            c.disconnect().await.expect("disconnect");
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
}

#[tokio::test]
async fn grants_can_be_made_listed_and_revoked() {
    let s = Server::start().await;
    let admin = s.session("t_grants").await;
    let svc = admin.create_service("t_grants", "svc", 4096).await.unwrap();

    admin
        .grant("bob", Some(&svc), &["service:read", "service:write"])
        .await
        .unwrap();
    let mine = admin.list_grants(Some("bob"), None).await.unwrap();
    assert_eq!(mine.len(), 1, "{mine:?}");
    assert_eq!(mine[0].service_id.as_deref(), Some(svc.as_str()));
    let mut perms = mine[0].permissions.clone();
    perms.sort();
    assert_eq!(perms, ["service:read", "service:write"]);

    admin
        .revoke("bob", Some(&svc), &["service:write"])
        .await
        .unwrap();
    let after = admin.list_grants(Some("bob"), None).await.unwrap();
    assert_eq!(after[0].permissions, ["service:read"]);

    // A bad request is refused by the SDK, an unknown service by the server.
    assert!(admin
        .grant("bob", Some(&svc), &["service:delete"])
        .await
        .is_err());
    assert!(admin
        .grant("bob", Some("no-such-service"), &["service:read"])
        .await
        .is_err());
}

#[tokio::test]
async fn a_client_without_a_grant_is_denied_until_granted_and_again_after_revocation() {
    use commy_sdk_rust::CommyError;
    let s = Server::start().await;
    let admin = s.session("t_denied").await;
    let svc = admin.create_service("t_denied", "svc", 4096).await.unwrap();

    // `bob` authenticates with no scopes: he holds nothing until an administrator grants it.
    let bob = s.client().with_dev_scopes(&[]);
    bob.connect().await.unwrap();
    bob.authenticate("t_denied", auth::api_key("x".to_string()))
        .await
        .unwrap();
    let denied = bob.open_remote(&svc).await.unwrap_err();
    assert!(
        matches!(denied, CommyError::PermissionDenied(_)),
        "{denied:?}"
    );

    admin
        .grant(bob.id(), Some(&svc), &["service:write"])
        .await
        .unwrap();
    bob.open_remote(&svc).await.expect("allowed once granted");
    bob.shared_write(&svc, 0, b"hi").await.unwrap();
    bob.flush_shared(&svc).await.unwrap();

    // Removing him from the service drops his mapping and moves the file.
    admin.revoke(bob.id(), Some(&svc), &[]).await.unwrap();
    let mut lost = false;
    for _ in 0..20 {
        if let SharedEvent::Loss(l) = bob.poll_shared_events(WAIT).await.unwrap() {
            assert_eq!(l.kind, commy_wire::LossKind::Revoked);
            lost = true;
            break;
        }
    }
    assert!(lost, "bob must be told he lost access");
    assert!(matches!(
        bob.shared_read(&svc, 0, 2).await,
        Err(CommyError::NotFound(_))
    ));
    let again = bob.open_remote(&svc).await.unwrap_err();
    assert!(
        matches!(again, CommyError::PermissionDenied(_)),
        "{again:?}"
    );
}

/// A tenant whose service `svc` is open to local mapping, and two admin sessions on it.
async fn local_rig(s: &Server, tenant: &str) -> (Arc<Client>, Arc<Client>, String) {
    let a = Arc::new(s.session(tenant).await);
    let b = Arc::new(s.session(tenant).await);
    let svc = a.create_service(tenant, "svc", 8192).await.unwrap();
    assert_eq!(
        a.open_shared(&svc).await.unwrap(),
        None,
        "local mapping is closed until an administrator opens it"
    );
    assert!(a.set_local_mapping(&svc, true).await.unwrap());
    (a, b, svc)
}

#[tokio::test]
async fn local_mode_shares_one_mapping_between_clients_and_with_remote_ones() {
    let s = Server::start().await;
    let (a, b, svc) = local_rig(&s, "t_local").await;
    let fa = a.open_shared(&svc).await.unwrap().expect("local mapping");
    let fb = b.open_shared(&svc).await.unwrap().expect("local mapping");
    assert_eq!(fa, fb);
    let r = Arc::new(s.session("t_local").await);
    r.open_remote(&svc).await.unwrap();

    a.shared_write(&svc, 10, b"local").await.unwrap();
    a.flush_shared(&svc).await.unwrap();
    assert!(b.poll_shared_updates(WAIT).await.unwrap(), "B must hear A");
    assert_eq!(b.shared_read(&svc, 10, 5).await.unwrap(), b"local");
    assert!(r.poll_shared_updates(WAIT).await.unwrap(), "R must hear A");
    assert_eq!(r.shared_read(&svc, 10, 5).await.unwrap(), b"local");

    // The remote writer's bytes land in the mapping both local clients share.
    r.shared_write(&svc, 100, b"remote").await.unwrap();
    r.flush_shared(&svc).await.unwrap();
    assert!(a.poll_shared_updates(WAIT).await.unwrap());
    assert_eq!(a.shared_read(&svc, 100, 6).await.unwrap(), b"remote");
    assert_eq!(b.shared_read(&svc, 100, 6).await.unwrap(), b"remote");

    // Closing the mapping stops future offers.
    assert!(!a.set_local_mapping(&svc, false).await.unwrap());
    let late = s.session("t_local").await;
    assert_eq!(late.open_shared(&svc).await.unwrap(), None);
}

#[tokio::test]
async fn local_mode_with_the_write_trap_reports_trapped_writes() {
    let s = Server::start().await;
    let (a, b, svc) = local_rig(&s, "t_trap").await;
    a.open_shared(&svc).await.unwrap().expect("local mapping");
    b.open_shared(&svc).await.unwrap().expect("local mapping");
    // SAFETY: only this test thread writes the mapping, through `shared_write`.
    unsafe { a.arm_shared_trap(&svc, 64) }.await.unwrap();

    a.shared_write(&svc, 5000, b"trapped").await.unwrap();
    a.flush_shared(&svc).await.unwrap();
    assert!(b.poll_shared_updates(WAIT).await.unwrap());
    assert_eq!(b.shared_read(&svc, 5000, 7).await.unwrap(), b"trapped");

    // A write by B reaches A, which must not echo it back as its own change.
    b.shared_write(&svc, 6000, b"from b").await.unwrap();
    b.flush_shared(&svc).await.unwrap();
    assert!(a.poll_shared_updates(WAIT).await.unwrap());
    assert_eq!(a.shared_read(&svc, 6000, 6).await.unwrap(), b"from b");
    a.flush_shared(&svc).await.unwrap();
    assert!(matches!(
        b.poll_shared_events(Duration::from_millis(500))
            .await
            .unwrap(),
        SharedEvent::None
    ));
}

#[tokio::test]
async fn local_mode_resize_is_filled_through_the_prepared_file() {
    let s = Server::start().await;
    let (a, b, svc) = local_rig(&s, "t_lresize").await;
    a.open_shared(&svc).await.unwrap().expect("local mapping");
    b.open_shared(&svc).await.unwrap().expect("local mapping");
    a.shared_write(&svc, 10, b"old layout").await.unwrap();
    a.flush_shared(&svc).await.unwrap();
    assert!(b.poll_shared_updates(WAIT).await.unwrap());
    a.request_rotation(&svc, RotateReason::Resize, Some(16384), None)
        .await
        .unwrap();

    let requester = async {
        let p = next_pending(&a).await;
        let RotationOutcome::Prepared(r) = a.drain_for_rotation(&p).await.unwrap() else {
            panic!("the requester must be asked to fill");
        };
        assert!(r.path.is_some(), "a local requester gets the file path");
        let mut fill = a.begin_fill(&p, &r).unwrap();
        let old = a.shared_read(&svc, 10, 10).await.unwrap();
        fill.write(4096, &old).unwrap();
        a.finish_fill(fill).await.unwrap()
    };
    let member = async {
        let p = next_pending(&b).await;
        b.drain_for_rotation(&p).await.unwrap()
    };
    let (ra, rb) = tokio::join!(requester, member);
    assert!(matches!(ra, RotationOutcome::Committed(_)));
    assert!(matches!(rb, RotationOutcome::Committed(_)));
    for c in [&a, &b] {
        assert_eq!(c.shared_read(&svc, 4096, 10).await.unwrap(), b"old layout");
        assert!(c.shared_read(&svc, 16380, 4).await.is_ok(), "new size");
    }
    // Both are still on local mappings of the NEW file.
    b.shared_write(&svc, 12000, b"after").await.unwrap();
    b.flush_shared(&svc).await.unwrap();
    assert!(a.poll_shared_updates(WAIT).await.unwrap());
    assert_eq!(a.shared_read(&svc, 12000, 5).await.unwrap(), b"after");
}

#[tokio::test]
async fn services_and_grants_survive_a_server_restart_and_so_does_a_revoke() {
    use commy_sdk_rust::CommyError;
    let mut s = Server::start().await;
    let admin = s.session("t_restart").await;
    let svc = admin
        .create_service("t_restart", "svc", 4096)
        .await
        .unwrap();
    admin
        .grant(
            "bob-restart",
            Some(&svc),
            &["service:read", "service:write"],
        )
        .await
        .unwrap();
    let bob_client = |s: &Server| {
        Client::with_id(s.url.clone(), "bob-restart")
            .with_tls_root_pem(s.cert_pem.clone())
            .with_dev_scopes(&[])
    };
    let bob = bob_client(&s);
    bob.connect().await.unwrap();
    bob.authenticate("t_restart", auth::api_key("x".to_string()))
        .await
        .unwrap();
    bob.open_remote(&svc).await.expect("allowed by the grant");
    drop((admin, bob));

    s.restart().await;

    // The same service id, and bob's grant is still there with no administrator acting.
    let admin = s.session("t_restart").await;
    assert_eq!(
        admin.get_service("t_restart", "svc").await.unwrap().id,
        svc,
        "the registry is durable"
    );
    let entries = admin.list_grants(Some("bob-restart"), None).await.unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    let bob = bob_client(&s);
    bob.connect().await.unwrap();
    bob.authenticate("t_restart", auth::api_key("x".to_string()))
        .await
        .unwrap();
    bob.open_remote(&svc)
        .await
        .expect("the grant survived the restart");

    // A revoke is durable too: a restart must not give the access back.
    admin
        .revoke(
            "bob-restart",
            Some(&svc),
            &["service:read", "service:write"],
        )
        .await
        .unwrap();
    drop((admin, bob));
    s.restart().await;
    let bob = bob_client(&s);
    bob.connect().await.unwrap();
    bob.authenticate("t_restart", auth::api_key("x".to_string()))
        .await
        .unwrap();
    let denied = bob.open_remote(&svc).await.unwrap_err();
    assert!(
        matches!(denied, CommyError::PermissionDenied(_)),
        "a revoked grant must stay revoked: {denied:?}"
    );
}

#[tokio::test]
async fn a_member_that_never_drains_is_dropped_as_a_straggler_and_the_resize_goes_on() {
    use commy_sdk_rust::CommyError;
    // A short drain barrier so the test does not wait the default 5 s.
    let s = Server::start_with(&[("COMMY_DRAIN_TIMEOUT_MS", "500")]).await;
    let (a, silent, svc) = rotation_rig(&s, "t_straggler").await;
    a.request_rotation(&svc, RotateReason::Resize, Some(16384), None)
        .await
        .unwrap();

    let requester = async {
        let p = next_pending(&a).await;
        let RotationOutcome::Prepared(r) = a.drain_for_rotation(&p).await.unwrap() else {
            panic!("the requester must be asked to fill");
        };
        let mut fill = a.begin_fill(&p, &r).unwrap();
        let old = a.shared_read(&svc, 10, 10).await.unwrap();
        fill.write(4096, &old).unwrap();
        a.finish_fill(fill).await.unwrap()
    };
    // The straggler sees the barrier open and then does nothing: it never calls drain_for_rotation.
    let straggler = async {
        let _pending = next_pending(&silent).await;
        for _ in 0..20 {
            if let SharedEvent::Loss(l) = silent.poll_shared_events(WAIT).await.unwrap() {
                return l;
            }
        }
        panic!("the straggler was never told it lost the service");
    };
    let (outcome, loss) = tokio::join!(requester, straggler);
    assert!(
        matches!(outcome, RotationOutcome::Committed(_)),
        "the resize must not wait for a member that does not answer"
    );
    assert_eq!(loss.kind, commy_wire::LossKind::RotationStraggler);
    // Its mapping is gone, and it cannot get back into the service's new file without a new join.
    assert!(matches!(
        silent.shared_read(&svc, 10, 10).await,
        Err(CommyError::NotFound(_))
    ));
    // Everyone who drained is on the new file and writes resume there.
    assert_eq!(a.shared_read(&svc, 4096, 10).await.unwrap(), b"old layout");
    a.shared_write(&svc, 12000, b"after").await.unwrap();
    a.flush_shared(&svc).await.unwrap();
}
