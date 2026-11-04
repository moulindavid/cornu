use std::time::Instant;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use crate::http::response_headers;
use crate::{Body, HttpClient, Method, Request, Response, SendError};

/// `ws://` and `wss://` URLs open a WebSocket instead of sending a request.
pub fn is_websocket_url(url: &str) -> bool {
    let url = url.trim_start().to_ascii_lowercase();
    url.starts_with("ws://") || url.starts_with("wss://")
}

/// The handshake is plain HTTP(S), so it goes through the same proxy, TLS settings and cookies.
fn http_url(url: &str) -> String {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("wss://") {
        format!("https://{}", &url[6..])
    } else if lower.starts_with("ws://") {
        format!("http://{}", &url[5..])
    } else {
        url.to_owned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsMessage {
    Text(String),
    Binary(Vec<u8>),
}

impl WsMessage {
    pub fn len(&self) -> usize {
        match self {
            WsMessage::Text(text) => text.len(),
            WsMessage::Binary(bytes) => bytes.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl From<WsMessage> for Message {
    fn from(message: WsMessage) -> Self {
        match message {
            WsMessage::Text(text) => Message::text(text),
            WsMessage::Binary(bytes) => Message::binary(bytes),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsCommand {
    Send(WsMessage),
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsEvent {
    Sent(WsMessage),
    Received(WsMessage),
    /// Always the last event once the connection ends cleanly.
    Closed {
        code: Option<u16>,
        reason: String,
    },
    /// The last event when the connection breaks.
    Failed(String),
}

pub struct WebSocket {
    stream: WebSocketStream<reqwest::Upgraded>,
}

impl HttpClient {
    /// Opens a WebSocket with the request's URL, query, headers and auth; the body is ignored.
    /// The returned response is the handshake, without a body.
    pub async fn connect_websocket(
        &self,
        request: &Request,
    ) -> Result<(Response, WebSocket), SendError> {
        let mut request = request.clone();
        request.method = Method::Get;
        request.body = Body::None;
        request.url = http_url(&request.url);
        let key = generate_key();
        let builder = self
            .prepare(&request)
            .await?
            // Upgrades only exist in HTTP/1.1.
            .version(reqwest::Version::HTTP_11)
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", &key);

        let started = Instant::now();
        let response = builder.send().await?;
        let status = response.status().as_u16();
        let headers = response_headers(&response);
        if status != 101 {
            let body = response.text().await.unwrap_or_default();
            let body: String = body.chars().take(200).collect();
            return Err(SendError::WebSocket(format!(
                "the server answered {status} instead of switching to WebSocket{}",
                if body.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", body.trim())
                }
            )));
        }
        let accept = response
            .headers()
            .get("sec-websocket-accept")
            .map(|value| value.as_bytes().to_vec());
        if accept.as_deref() != Some(derive_accept_key(key.as_bytes()).as_bytes()) {
            return Err(SendError::WebSocket(
                "the server did not accept the WebSocket key".into(),
            ));
        }
        let elapsed = started.elapsed();
        let upgraded = response.upgrade().await?;
        let stream = WebSocketStream::from_raw_socket(upgraded, Role::Client, None).await;
        let handshake = Response {
            status,
            headers,
            body: Vec::new(),
            elapsed,
        };
        Ok((handshake, WebSocket { stream }))
    }
}

impl WebSocket {
    /// Sends what `commands` asks and reports everything that happens to `on_event`, until the
    /// connection ends. Dropping the sender of `commands` closes the connection.
    pub async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<WsCommand>,
        mut on_event: impl FnMut(WsEvent),
    ) {
        let mut closing = false;
        let mut closed = None;
        loop {
            tokio::select! {
                command = commands.recv(), if !closing => match command {
                    Some(WsCommand::Send(message)) => {
                        match self.stream.send(message.clone().into()).await {
                            Ok(()) => on_event(WsEvent::Sent(message)),
                            Err(error) => return on_event(WsEvent::Failed(error.to_string())),
                        }
                    }
                    Some(WsCommand::Close) | None => {
                        closing = true;
                        let frame = CloseFrame {
                            code: CloseCode::Normal,
                            reason: "".into(),
                        };
                        if let Err(error) = self.stream.close(Some(frame)).await {
                            return on_event(WsEvent::Failed(error.to_string()));
                        }
                    }
                },
                message = self.stream.next() => match message {
                    Some(Ok(Message::Text(text))) => {
                        on_event(WsEvent::Received(WsMessage::Text(text.to_string())));
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        on_event(WsEvent::Received(WsMessage::Binary(bytes.to_vec())));
                    }
                    // Keep reading: the closing handshake completes on the next poll.
                    Some(Ok(Message::Close(frame))) => {
                        closing = true;
                        closed = Some(WsEvent::Closed {
                            code: frame.as_ref().map(|frame| u16::from(frame.code)),
                            reason: frame.map(|frame| frame.reason.to_string()).unwrap_or_default(),
                        });
                    }
                    Some(Ok(_)) => {}
                    None | Some(Err(WsError::ConnectionClosed | WsError::AlreadyClosed)) => {
                        return on_event(closed.unwrap_or(WsEvent::Closed {
                            code: None,
                            reason: String::new(),
                        }));
                    }
                    Some(Err(error)) => {
                        return on_event(closed.unwrap_or(WsEvent::Failed(error.to_string())));
                    }
                },
            }
        }
    }
}
