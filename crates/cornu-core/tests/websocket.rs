use std::sync::{Arc, Mutex};
use std::time::Duration;

use cornu_core::{
    Auth, ClientOptions, HttpClient, Method, Request, SendError, WsCommand, WsEvent, WsMessage,
    is_websocket_url,
};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request as Handshake, Response};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Echoes every message; `bye` makes it close with code 4000. Returns its `ws://` URL and the
/// request line and authorization header of the last handshake.
// The callback signature, with its large error type, is tungstenite's.
#[allow(clippy::result_large_err)]
async fn echo_server() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/chat", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = seen.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let record = record.clone();
            tokio::spawn(async move {
                let callback = |request: &Handshake, response: Response| {
                    let mut seen = record.lock().unwrap();
                    seen.push(request.uri().to_string());
                    if let Some(auth) = request.headers().get("authorization") {
                        seen.push(auth.to_str().unwrap().to_owned());
                    }
                    Ok(response)
                };
                let mut socket = tokio_tungstenite::accept_hdr_async(stream, callback)
                    .await
                    .unwrap();
                while let Some(Ok(message)) = socket.next().await {
                    match message {
                        Message::Text(text) if text.as_str() == "bye" => {
                            let frame = CloseFrame {
                                code: CloseCode::from(4000),
                                reason: "see you".into(),
                            };
                            let _ = socket.close(Some(frame)).await;
                        }
                        Message::Text(_) | Message::Binary(_) => {
                            socket.send(message).await.unwrap();
                        }
                        _ => {}
                    }
                }
            });
        }
    });
    (url, seen)
}

/// Runs a session, sending `commands` one after the other, each once the previous one is
/// answered, and returns every event.
async fn session(client: &HttpClient, request: &Request, commands: Vec<WsCommand>) -> Vec<WsEvent> {
    let (_, socket) = client.connect_websocket(request).await.unwrap();
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(socket.run(command_rx, move |event| {
        let _ = event_tx.send(event);
    }));
    let mut events = Vec::new();
    for command in commands {
        let expects_echo = matches!(command, WsCommand::Send(WsMessage::Text(ref t)) if t != "bye")
            || matches!(command, WsCommand::Send(WsMessage::Binary(_)));
        command_tx.send(command).unwrap();
        let wanted = if expects_echo { 2 } else { 1 };
        for _ in 0..wanted {
            events.push(event_rx.recv().await.unwrap());
        }
    }
    drop(command_tx);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    while let Ok(event) = event_rx.try_recv() {
        events.push(event);
    }
    events
}

#[test]
fn websocket_urls_are_recognized() {
    assert!(is_websocket_url("ws://localhost/chat"));
    assert!(is_websocket_url("  WSS://api.example/socket"));
    assert!(!is_websocket_url("https://api.example"));
    assert!(!is_websocket_url("{{base}}/socket"));
}

#[tokio::test]
async fn messages_are_echoed_with_query_and_auth_on_the_handshake() {
    let (url, seen) = echo_server().await;
    let client = HttpClient::new(&ClientOptions::default()).unwrap();
    let request = Request::new(Method::Post, url)
        .with_query("room", "42")
        .with_auth(Auth::Bearer {
            token: "abc".into(),
        });

    let (handshake, _) = client.connect_websocket(&request).await.unwrap();
    assert_eq!(handshake.status, 101);
    assert!(handshake.header("sec-websocket-accept").is_some());

    let text = WsMessage::Text("hello".into());
    let binary = WsMessage::Binary(vec![0, 159, 255]);
    let events = session(
        &client,
        &request,
        vec![
            WsCommand::Send(text.clone()),
            WsCommand::Send(binary.clone()),
            WsCommand::Close,
        ],
    )
    .await;
    assert_eq!(
        events,
        [
            WsEvent::Sent(text.clone()),
            WsEvent::Received(text),
            WsEvent::Sent(binary.clone()),
            WsEvent::Received(binary),
            WsEvent::Closed {
                code: Some(1000),
                reason: String::new()
            },
        ]
    );
    assert_eq!(
        seen.lock().unwrap()[..2],
        ["/chat?room=42".to_owned(), "Bearer abc".to_owned()]
    );
}

#[tokio::test]
async fn server_close_code_and_reason_are_reported() {
    let (url, _) = echo_server().await;
    let client = HttpClient::new(&ClientOptions::default()).unwrap();
    let events = session(
        &client,
        &Request::new(Method::Get, url),
        vec![WsCommand::Send(WsMessage::Text("bye".into()))],
    )
    .await;
    assert_eq!(
        events,
        [
            WsEvent::Sent(WsMessage::Text("bye".into())),
            WsEvent::Closed {
                code: Some(4000),
                reason: "see you".into()
            },
        ]
    );
}

#[tokio::test]
async fn the_request_timeout_does_not_cut_an_open_socket() {
    let (url, _) = echo_server().await;
    let options = ClientOptions {
        timeout: Some(Duration::from_millis(300)),
        ..Default::default()
    };
    let client = HttpClient::new(&options).unwrap();
    let (_, socket) = client
        .connect_websocket(&Request::new(Method::Get, url))
        .await
        .unwrap();
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    tokio::spawn(socket.run(command_rx, move |event| {
        let _ = event_tx.send(event);
    }));
    tokio::time::sleep(Duration::from_millis(600)).await;
    command_tx
        .send(WsCommand::Send(WsMessage::Text("still there".into())))
        .unwrap();
    event_rx.recv().await.unwrap();
    assert_eq!(
        event_rx.recv().await.unwrap(),
        WsEvent::Received(WsMessage::Text("still there".into()))
    );
}

#[tokio::test]
async fn a_plain_http_answer_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(404).set_body_string("no socket here"))
        .mount(&server)
        .await;
    let client = HttpClient::new(&ClientOptions::default()).unwrap();
    let url = server.uri().replace("http://", "ws://");
    let error = client
        .connect_websocket(&Request::new(Method::Get, url))
        .await
        .err()
        .unwrap();
    assert!(
        matches!(&error, SendError::WebSocket(message) if message.contains("404") && message.contains("no socket here")),
        "{error}"
    );
}
