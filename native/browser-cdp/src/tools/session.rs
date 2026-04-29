//! CDP WebSocket session

use std::time::Duration;
use anyhow::{anyhow, Context, Result};
use futures::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

pub struct CdpSession {
    pub ws: WsStream,
    pub next_id: u64,
}

impl CdpSession {
    pub async fn connect(ws_url: &str) -> Result<Self> {
        let (ws, _) = connect_async(ws_url).await
            .with_context(|| format!("Failed to connect CDP WebSocket at {}", ws_url))?;
        
        let mut session = Self { ws, next_id: 1 };
        
        // Enable Page and Runtime domains
        session.call("Page.enable", serde_json::json!({})).await?;
        session.call("Runtime.enable", serde_json::json!({})).await?;
        
        Ok(session)
    }
    
    pub async fn call(&mut self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let id = self.next_id;
        self.next_id += 1;

        let req = serde_json::json!({"id": id, "method": method, "params": params});
        self.ws
            .send(Message::Text(req.to_string().into()))
            .await
            .context("Failed to send CDP command")?;

        loop {
            match tokio::time::timeout(Duration::from_secs(15), self.ws.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    let parsed: serde_json::Value =
                        serde_json::from_str(&text).context("CDP response is not valid JSON")?;
                    if parsed.get("id").and_then(|v| v.as_u64()) == Some(id) {
                        if let Some(error) = parsed.get("error") {
                            return Err(anyhow!("CDP error for {}: {}", method, error));
                        }
                        return Ok(parsed
                            .get("result")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null));
                    }
                }
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(e))) => return Err(anyhow!("CDP WebSocket error: {}", e)),
                Ok(None) => return Err(anyhow!("CDP WebSocket closed unexpectedly")),
                Err(_) => {
                    return Err(anyhow!(
                        "CDP command '{}' timed out after 15 seconds",
                        method
                    ))
                }
            }
        }
    }
}
