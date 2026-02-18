//! WebSocket connection management

use crate::error::{CommyError, Result};
use crate::message::{ClientMessage, ServerMessage};
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// Connection state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Authenticated,
    Closing,
}

/// Manages WebSocket connection
pub struct Connection {
    state: Arc<RwLock<ConnectionState>>,
    tx: mpsc::UnboundedSender<ClientMessage>,
    rx: Arc<RwLock<mpsc::UnboundedReceiver<ServerMessage>>>,
}

impl Connection {
    /// Create a new connection
    pub async fn new(url: &str) -> Result<Self> {
        let (ws_stream, _) = connect_async(url)
            .await
            .map_err(|e| CommyError::WebSocketError(e.to_string()))?;

        let (mut write, mut read) = ws_stream.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<ClientMessage>();
        let (server_tx, server_rx) = mpsc::unbounded_channel::<ServerMessage>();

        // Spawn tasks to handle message routing
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let Ok(serialized) = serde_json::to_string(&msg) {
                    let _ = write.send(Message::Text(serialized)).await;
                }
            }
        });

        tokio::spawn(async move {
            while let Some(Ok(msg)) = read.next().await {
                if let Message::Text(text) = msg {
                    match serde_json::from_str::<ServerMessage>(&text) {
                        Ok(server_msg) => {
                            let _ = server_tx.send(server_msg);
                        }
                        Err(e) => {
                            eprintln!("[Client] Failed to deserialize ServerMessage: {}", e);
                            eprintln!("[Client] Raw message: {}", text);
                        }
                    }
                }
            }
        });

        Ok(Self {
            state: Arc::new(RwLock::new(ConnectionState::Connected)),
            tx,
            rx: Arc::new(RwLock::new(server_rx)),
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

    /// Get current connection state
    pub async fn state(&self) -> ConnectionState {
        *self.state.read().await
    }

    /// Set connection state
    pub async fn set_state(&self, state: ConnectionState) {
        *self.state.write().await = state;
    }

    /// Check if connected
    pub async fn is_connected(&self) -> bool {
        matches!(
            *self.state.read().await,
            ConnectionState::Connected | ConnectionState::Authenticated
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_state() {
        let state = ConnectionState::Connected;
        assert!(matches!(state, ConnectionState::Connected));
    }
}
