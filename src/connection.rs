// SPDX-License-Identifier: MIT
//! WebSocket connection management

use crate::error::{CommyError, Result};
use crate::message::{ClientMessage, ServerMessage};
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::{connect_async_tls_with_config, tungstenite::Message, Connector};

/// Connection state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Authenticated,
    Closing,
}

/// The WebSocket frame for an outbound message: range frames are binary, everything else JSON
/// text. `None` when the message cannot be serialized.
fn encode_outbound(msg: &ClientMessage) -> Option<Message> {
    match msg {
        ClientMessage::Range(frame) => Some(Message::Binary(frame.encode().into())),
        other => serde_json::to_string(other)
            .ok()
            .map(|s| Message::Text(s.into())),
    }
}

/// The message carried by an inbound WebSocket frame. A binary frame must be a valid range frame;
/// a refused one is dropped (never partly applied). Text must be a known `ServerMessage`.
fn decode_inbound(msg: Message) -> Option<ServerMessage> {
    match msg {
        Message::Text(text) => match serde_json::from_str::<ServerMessage>(&text) {
            Ok(m) => Some(m),
            Err(e) => {
                eprintln!("[Client] Failed to deserialize ServerMessage: {}", e);
                eprintln!("[Client] Raw message: {}", text);
                None
            }
        },
        Message::Binary(bytes) => match commy_wire::RangeFrame::decode(&bytes) {
            Ok(frame) => Some(ServerMessage::Range(frame)),
            Err(e) => {
                eprintln!("[Client] Refused binary frame: {e:?}");
                None
            }
        },
        _ => None,
    }
}

/// A client certificate chain and its private key, both PEM: what a connection presents to a server
/// that asks for client authentication (`AuthCredentials::Mtls`). The key is never shown by `Debug`,
/// is zeroed when the identity is dropped, and the type is not `Clone`, so no untracked copy of the
/// key exists.
pub struct ClientIdentity {
    cert_chain_pem: Vec<u8>,
    key_pem: crate::message::Secret<zeroize::Zeroizing<Vec<u8>>>,
}

impl ClientIdentity {
    /// An identity from the PEM certificate chain (leaf first) and the PEM private key.
    pub fn new(cert_chain_pem: impl Into<Vec<u8>>, key_pem: impl Into<Vec<u8>>) -> Self {
        Self {
            cert_chain_pem: cert_chain_pem.into(),
            key_pem: crate::message::Secret::new(zeroize::Zeroizing::new(key_pem.into())),
        }
    }
}

impl std::fmt::Debug for ClientIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientIdentity")
            .field(
                "cert_chain_pem",
                &format_args!("{} bytes", self.cert_chain_pem.len()),
            )
            .field("key_pem", &"<redacted>")
            .finish()
    }
}

/// A TLS client config: it trusts only the certificates in `root_pem` (the system roots when
/// `None`), and presents `identity` when one is given.
fn client_config(
    root_pem: Option<&[u8]>,
    identity: Option<&ClientIdentity>,
) -> Result<rustls::ClientConfig> {
    use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
    let mut roots = rustls::RootCertStore::empty();
    match root_pem {
        Some(pem) => {
            for cert in CertificateDer::pem_slice_iter(pem) {
                let cert =
                    cert.map_err(|e| CommyError::WebSocketError(format!("bad root PEM: {e}")))?;
                roots.add(cert).map_err(|e| {
                    CommyError::WebSocketError(format!("bad root certificate: {e}"))
                })?;
            }
            if roots.is_empty() {
                return Err(CommyError::WebSocketError(
                    "no certificate in the root PEM".to_string(),
                ));
            }
        }
        None => {
            let native = rustls_native_certs::load_native_certs();
            roots.add_parsable_certificates(native.certs);
            if roots.is_empty() && !native.errors.is_empty() {
                return Err(CommyError::WebSocketError(format!(
                    "no system root certificates found ({} load errors, first: {})",
                    native.errors.len(),
                    native.errors[0]
                )));
            }
            if roots.is_empty() {
                return Err(CommyError::WebSocketError(
                    "no system root certificates found".to_string(),
                ));
            }
        }
    }
    let builder = rustls::ClientConfig::builder().with_root_certificates(roots);
    match identity {
        None => Ok(builder.with_no_client_auth()),
        Some(id) => {
            let chain = CertificateDer::pem_slice_iter(&id.cert_chain_pem)
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| {
                    CommyError::WebSocketError(format!("bad client certificate PEM: {e}"))
                })?;
            if chain.is_empty() {
                return Err(CommyError::WebSocketError(
                    "no certificate in the client identity PEM".to_string(),
                ));
            }
            let key = PrivateKeyDer::from_pem_slice(id.key_pem.expose()).map_err(|e| {
                CommyError::WebSocketError(format!("bad client private key PEM: {e}"))
            })?;
            builder
                .with_client_auth_cert(chain, key)
                .map_err(|e| CommyError::WebSocketError(format!("bad client identity: {e}")))
        }
    }
}

/// Manages WebSocket connection
pub struct Connection {
    state: Arc<RwLock<ConnectionState>>,
    tx: mpsc::UnboundedSender<ClientMessage>,
    rx: Arc<RwLock<mpsc::UnboundedReceiver<ServerMessage>>>,
    /// Pushes a waiting call read but did not want, oldest first; `poll_shared_events` takes them
    /// before anything newer in `rx`.
    kept: Arc<std::sync::Mutex<std::collections::VecDeque<ServerMessage>>>,
}

impl Connection {
    /// Create a new connection
    pub async fn new(url: &str) -> Result<Self> {
        Self::new_with_roots(url, None).await
    }

    /// Create a new connection. With `root_pem`, trust exactly those certificates instead of the
    /// system roots (a self-signed development server).
    pub async fn new_with_roots(url: &str, root_pem: Option<&[u8]>) -> Result<Self> {
        Self::new_with_tls(url, root_pem, None).await
    }

    /// Create a new connection that presents `identity` as its TLS client certificate (the
    /// identity `AuthCredentials::Mtls` names), with the same `root_pem` rule as
    /// [`Connection::new_with_roots`].
    pub async fn new_with_tls(
        url: &str,
        root_pem: Option<&[u8]>,
        identity: Option<&ClientIdentity>,
    ) -> Result<Self> {
        let connector = if root_pem.is_some() || identity.is_some() {
            Some(Connector::Rustls(Arc::new(client_config(
                root_pem, identity,
            )?)))
        } else {
            None
        };
        let (ws_stream, _) = connect_async_tls_with_config(url, None, false, connector)
            .await
            .map_err(|e| CommyError::WebSocketError(e.to_string()))?;

        let (mut write, mut read) = ws_stream.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<ClientMessage>();
        let (server_tx, server_rx) = mpsc::unbounded_channel::<ServerMessage>();

        // Spawn tasks to handle message routing
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let Some(frame) = encode_outbound(&msg) {
                    let _ = write.send(frame).await;
                }
            }
        });

        tokio::spawn(async move {
            while let Some(Ok(msg)) = read.next().await {
                if let Some(server_msg) = decode_inbound(msg) {
                    let _ = server_tx.send(server_msg);
                }
            }
        });

        Ok(Self {
            state: Arc::new(RwLock::new(ConnectionState::Connected)),
            tx,
            rx: Arc::new(RwLock::new(server_rx)),
            kept: Arc::default(),
        })
    }

    /// Send a message to the server
    pub async fn send(&self, message: ClientMessage) -> Result<()> {
        self.tx
            .send(message)
            .map_err(|e| CommyError::ChannelError(format!("Failed to send message: {}", e)))?;
        Ok(())
    }

    /// Receive a message from the server
    pub async fn recv(&self) -> Result<Option<ServerMessage>> {
        let mut rx = self.rx.write().await;
        Ok(rx.recv().await)
    }

    /// Keep a push that a waiting call read but is not its reply, for `take_kept`.
    pub(crate) fn keep(&self, msg: ServerMessage) {
        if let Ok(mut q) = self.kept.lock() {
            q.push_back(msg);
        }
    }

    /// The oldest kept push, if any.
    pub(crate) fn take_kept(&self) -> Option<ServerMessage> {
        self.kept.lock().ok().and_then(|mut q| q.pop_front())
    }

    /// Get current connection state
    pub async fn state(&self) -> ConnectionState {
        *self.state.read().await
    }

    /// Create a mock connection for testing that uses in-memory channels.
    ///
    /// Returns `(conn, server_tx, client_rx)` where:
    /// - `conn` is the mock `Connection` ready to be injected into a `Client`
    /// - `server_tx` is used by the test to **inject** `ServerMessage` responses
    /// - `client_rx` is used by the test to **observe** `ClientMessage`s sent by the client
    #[cfg(test)]
    pub fn new_for_test() -> (
        Self,
        mpsc::UnboundedSender<ServerMessage>,
        mpsc::UnboundedReceiver<ClientMessage>,
    ) {
        let (server_inbound_tx, server_inbound_rx) = mpsc::unbounded_channel::<ServerMessage>();
        let (client_outbound_tx, client_outbound_rx) = mpsc::unbounded_channel::<ClientMessage>();

        let conn = Self {
            state: Arc::new(RwLock::new(ConnectionState::Connected)),
            tx: client_outbound_tx,
            rx: Arc::new(RwLock::new(server_inbound_rx)),
            kept: Arc::default(),
        };

        (conn, server_inbound_tx, client_outbound_rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A loopback WSS server with a fresh self-signed certificate for 127.0.0.1; returns its URL
    /// and the certificate PEM.
    async fn tls_server() -> (String, String) {
        use rustls::pki_types::{pem::PemObject, PrivateKeyDer};
        let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()]).unwrap();
        let pem = cert.cert.pem();
        let certs = vec![cert.cert.der().clone()];
        let key =
            PrivateKeyDer::from_pem_slice(cert.signing_key.serialize_pem().as_bytes()).unwrap();
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    if let Ok(tls) = acceptor.accept(tcp).await {
                        if let Ok(mut ws) = tokio_tungstenite::accept_async(tls).await {
                            let _ = ws.send(Message::Text("{\"type\":\"Pong\"}".into())).await;
                            let _ = ws.next().await;
                        }
                    }
                });
            }
        });
        (format!("wss://127.0.0.1:{port}"), pem)
    }

    #[tokio::test]
    async fn self_signed_server_is_trusted_only_with_its_root() {
        let (url, pem) = tls_server().await;
        assert!(
            Connection::new_with_roots(&url, Some(pem.as_bytes()))
                .await
                .is_ok(),
            "the pinned root must be accepted"
        );
        assert!(
            Connection::new(&url).await.is_err(),
            "without the root a self-signed certificate must be refused"
        );
    }

    #[tokio::test]
    async fn another_servers_root_is_not_trusted() {
        let (url, _) = tls_server().await;
        let (_, other_pem) = tls_server().await;
        assert!(Connection::new_with_roots(&url, Some(other_pem.as_bytes()))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn a_root_pem_with_no_certificate_is_an_error() {
        let (url, _) = tls_server().await;
        let e = Connection::new_with_roots(&url, Some(b"not a pem")).await;
        assert!(matches!(e, Err(CommyError::WebSocketError(m)) if m.contains("no certificate")));
    }

    /// A loopback WSS server that REQUIRES a client certificate signed by its own CA. Returns the
    /// URL, the server certificate PEM (the root the client must trust), the client identity, and a
    /// channel that reports, per accepted handshake, the DER of the certificate the client
    /// presented (`None`: the handshake was refused).
    async fn mtls_server() -> (
        String,
        String,
        ClientIdentity,
        tokio::sync::mpsc::UnboundedReceiver<Option<Vec<u8>>>,
    ) {
        use rustls::pki_types::{pem::PemObject, PrivateKeyDer};
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let issuer = rcgen::Issuer::from_params(&ca_params, &ca_key);
        let client_key = rcgen::KeyPair::generate().unwrap();
        let client = rcgen::CertificateParams::new(vec!["client.test".to_string()])
            .unwrap()
            .signed_by(&client_key, &issuer)
            .unwrap();
        let server = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()]).unwrap();

        let mut client_roots = rustls::RootCertStore::empty();
        client_roots.add(ca.der().clone()).unwrap();
        let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(client_roots))
            .build()
            .unwrap();
        let key =
            PrivateKeyDer::from_pem_slice(server.signing_key.serialize_pem().as_bytes()).unwrap();
        let config = rustls::ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(vec![server.cert.der().clone()], key)
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (seen_tx, seen_rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                let seen_tx = seen_tx.clone();
                tokio::spawn(async move {
                    match acceptor.accept(tcp).await {
                        Ok(tls) => {
                            let presented = tls
                                .get_ref()
                                .1
                                .peer_certificates()
                                .and_then(|c| c.first())
                                .map(|c| c.as_ref().to_vec());
                            let _ = seen_tx.send(presented);
                            if let Ok(mut ws) = tokio_tungstenite::accept_async(tls).await {
                                let _ = ws.next().await;
                            }
                        }
                        Err(_) => {
                            let _ = seen_tx.send(None);
                        }
                    }
                });
            }
        });
        let identity = ClientIdentity::new(client.pem(), client_key.serialize_pem());
        (
            format!("wss://127.0.0.1:{port}"),
            server.cert.pem(),
            identity,
            seen_rx,
        )
    }

    #[tokio::test]
    async fn the_connection_presents_its_client_identity_and_a_server_that_requires_one_refuses_none(
    ) {
        use rustls::pki_types::{pem::PemObject, CertificateDer};
        let (url, root, identity, mut seen) = mtls_server().await;

        let conn = Connection::new_with_tls(&url, Some(root.as_bytes()), Some(&identity)).await;
        assert!(
            conn.is_ok(),
            "an identity the server's CA signed is accepted"
        );
        let presented = seen
            .recv()
            .await
            .unwrap()
            .expect("the server saw a certificate");
        let sent = CertificateDer::pem_slice_iter(&identity.cert_chain_pem)
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(
            presented,
            sent.as_ref(),
            "the server saw exactly our certificate"
        );

        // No identity: the handshake is refused on the server side.
        assert!(Connection::new_with_roots(&url, Some(root.as_bytes())).await.is_err());
        assert_eq!(seen.recv().await.unwrap(), None);
    }

    #[test]
    fn system_roots_with_a_bad_identity_name_the_identity_error() {
        let bad = ClientIdentity::new(&b"not a pem"[..], &b"not a pem"[..]);
        // Config construction only: a handshake on system roots is not constructible locally.
        if let Err(e) = client_config(None, Some(&bad)) {
            assert!(matches!(e, CommyError::WebSocketError(_)));
        } else {
            panic!("a bad identity must not build a config");
        }
    }

    #[tokio::test]
    async fn a_bad_client_identity_is_an_error_before_connecting() {
        let (url, root, _, _) = mtls_server().await;
        for id in [
            ClientIdentity::new(&b"not a pem"[..], &b"not a pem"[..]),
            ClientIdentity::new(Vec::new(), Vec::new()),
        ] {
            let e = Connection::new_with_tls(&url, Some(root.as_bytes()), Some(&id)).await;
            assert!(
                matches!(e, Err(CommyError::WebSocketError(_))),
                "expected a WebSocketError, got ok={}",
                e.is_ok()
            );
        }
    }

    #[test]
    fn the_identitys_debug_never_shows_the_key() {
        let id = ClientIdentity::new(&b"cert"[..], &b"PRIVATE-KEY-BYTES"[..]);
        let shown = format!("{id:?}");
        assert!(!shown.contains("PRIVATE-KEY-BYTES"), "{shown}");
        assert!(shown.contains("redacted"));
    }

    #[test]
    fn the_identitys_key_is_zeroizing_secret_storage_and_not_clone() {
        fn zeroizing(_: &crate::message::Secret<zeroize::Zeroizing<Vec<u8>>>) {}
        let id = ClientIdentity::new(&b"cert"[..], &b"k"[..]);
        zeroizing(&id.key_pem);
        // Wiping zeroes the bytes in place; the drop does the same through `Zeroizing`.
        let mut key = zeroize::Zeroizing::new(b"KEY".to_vec());
        zeroize::Zeroize::zeroize(&mut *key);
        assert!(key.is_empty());
        // `ClientIdentity: !Clone`: if this became Clone both impls would apply and not compile.
        trait AmbiguousIfClone<A> { fn probe() {} }
        impl<T: ?Sized> AmbiguousIfClone<()> for T {}
        impl<T: Clone> AmbiguousIfClone<u8> for T {}
        let _ = <ClientIdentity as AmbiguousIfClone<_>>::probe;
    }

    #[test]
    fn test_connection_state() {
        let state = ConnectionState::Connected;
        assert!(matches!(state, ConnectionState::Connected));
    }

    #[test]
    fn test_connection_state_disconnected() {
        let state = ConnectionState::Disconnected;
        assert!(matches!(state, ConnectionState::Disconnected));
        assert_ne!(state, ConnectionState::Connected);
    }

    #[test]
    fn test_connection_state_is_connected_pattern() {
        // Only Connected and Authenticated are considered "is_connected"
        assert!(matches!(
            ConnectionState::Connected,
            ConnectionState::Connected | ConnectionState::Authenticated
        ));
        assert!(matches!(
            ConnectionState::Authenticated,
            ConnectionState::Connected | ConnectionState::Authenticated
        ));
        assert!(!matches!(
            ConnectionState::Disconnected,
            ConnectionState::Connected | ConnectionState::Authenticated
        ));
        assert!(!matches!(
            ConnectionState::Connecting,
            ConnectionState::Connected | ConnectionState::Authenticated
        ));
        assert!(!matches!(
            ConnectionState::Closing,
            ConnectionState::Connected | ConnectionState::Authenticated
        ));
    }

    #[test]
    fn test_connection_state_equality() {
        assert_eq!(ConnectionState::Connected, ConnectionState::Connected);
        assert_ne!(ConnectionState::Connected, ConnectionState::Disconnected);
        assert_ne!(ConnectionState::Authenticated, ConnectionState::Connected);
    }

    #[tokio::test]
    async fn test_send_enqueues_message_on_client_rx() {
        use crate::message::ClientMessage;

        let (conn, _server_tx, mut client_rx) = Connection::new_for_test();

        conn.send(ClientMessage::Heartbeat {
            client_id: "test_client".to_string(),
            request_id: None,
        })
        .await
        .expect("send should succeed");

        let received = client_rx.try_recv().expect("message should be in channel");
        assert!(
            matches!(received, ClientMessage::Heartbeat { .. }),
            "Expected Heartbeat, got {:?}",
            received
        );
    }

    #[tokio::test]
    async fn test_recv_returns_pre_loaded_server_message() {
        use crate::message::ServerMessage;

        let (conn, server_tx, _client_rx) = Connection::new_for_test();

        server_tx
            .send(ServerMessage::Heartbeat {
                timestamp: "2025-01-01T00:00:00Z".to_string(),
                request_id: None,
            })
            .expect("pre-loading message should succeed");

        let result = conn.recv().await.expect("recv should not error");
        assert!(
            matches!(result, Some(ServerMessage::Heartbeat { .. })),
            "Expected Heartbeat ServerMessage, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_recv_on_closed_channel_returns_ok_none() {
        let (conn, server_tx, _client_rx) = Connection::new_for_test();

        // Drop the send half — channel is now closed from the server side
        drop(server_tx);

        let result = conn
            .recv()
            .await
            .expect("recv should not error even when channel closed");
        assert!(
            result.is_none(),
            "Closed channel should return Ok(None), got {:?}",
            result
        );
    }
}

#[cfg(test)]
mod codec_tests {
    use super::*;
    use commy_wire::RangeFrame;

    fn frame() -> RangeFrame {
        RangeFrame {
            file_id: 3,
            seq: 9,
            offset: 16,
            data: vec![1, 2, 3],
        }
    }

    #[test]
    fn range_frames_go_out_as_binary_and_come_back_equal() {
        let out = encode_outbound(&ClientMessage::Range(frame())).unwrap();
        let Message::Binary(bytes) = out.clone() else {
            panic!("range frame must be binary");
        };
        assert_eq!(bytes.len(), commy_wire::HEADER_LEN + 3);
        match decode_inbound(out) {
            Some(ServerMessage::Range(f)) => assert_eq!(f, frame()),
            other => panic!("expected Range, got {other:?}"),
        }
    }

    #[test]
    fn control_messages_go_out_as_json_text() {
        let msg = ClientMessage::FlushEnd(commy_wire::FlushEnd {
            request_id: "r".to_string(),
            file_id: 3,
        });
        let Some(Message::Text(t)) = encode_outbound(&msg) else {
            panic!("control message must be text");
        };
        assert!(t.contains("\"FlushEnd\""));
    }

    #[test]
    fn refused_binary_frames_are_dropped() {
        // truncated, empty payload (len 0), and unknown kind
        assert!(decode_inbound(Message::Binary(vec![1u8; 10].into())).is_none());
        let mut empty = frame().encode();
        empty.truncate(commy_wire::HEADER_LEN);
        empty[21..25].copy_from_slice(&0u32.to_le_bytes());
        assert!(decode_inbound(Message::Binary(empty.into())).is_none());
        let mut bad_kind = frame().encode();
        bad_kind[0] = 2;
        assert!(decode_inbound(Message::Binary(bad_kind.into())).is_none());
    }

    #[test]
    fn ping_and_close_are_ignored_and_bad_json_is_dropped() {
        assert!(decode_inbound(Message::Ping(vec![].into())).is_none());
        assert!(decode_inbound(Message::Text("not json".into())).is_none());
    }
}
