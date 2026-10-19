//! End-to-end: the SDK against a real Commy server over WSS with a real self-signed certificate.
//!
//! Built only with `--features e2e`; `COMMY_BIN` must name a built `commy` binary (CI builds one from
//! ciresnave/commy). The server runs in a temporary directory, on a free loopback port, with
//! development authentication on (the only way to authenticate today).

use commy_sdk_rust::client::SharedEvent;
use commy_sdk_rust::{auth, Client};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Server {
    child: Child,
    _dir: tempfile::TempDir,
    url: String,
    cert_pem: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    async fn start() -> Self {
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
        let child = Command::new(bin)
            .current_dir(dir.path())
            .env("COMMY_LISTEN_ADDR", "127.0.0.1")
            .env("COMMY_LISTEN_PORT", port.to_string())
            .env("COMMY_TLS_CERT_PATH", dir.path().join("cert.pem"))
            .env("COMMY_TLS_KEY_PATH", dir.path().join("key.pem"))
            .env("COMMY_ALLOW_DEV_AUTH", "1")
            // Anything not starting with postgresql:// or mysql:// selects in-memory auth storage.
            .env("DATABASE_URL", "memory")
            .stdout(std::fs::File::create(dir.path().join("out.log")).unwrap())
            .stderr(std::fs::File::create(dir.path().join("err.log")).unwrap())
            .stdin(Stdio::null())
            .spawn()
            .expect("start the commy server");
        let mut server = Server {
            child,
            _dir: dir,
            url: format!("wss://127.0.0.1:{port}"),
            cert_pem,
        };
        // Ready when a client can connect.
        for _ in 0..100 {
            if let Some(status) = server.child.try_wait().unwrap() {
                panic!("server exited early ({status}): {}", server.logs());
            }
            let probe = server.client();
            if probe.connect().await.is_ok() {
                return server;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("server never accepted a connection: {}", server.logs());
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
