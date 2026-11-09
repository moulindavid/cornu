use std::time::Duration;

use cornu_core::{
    ClientOptions, HttpClient, Method, Request, Response, SendError, SseEvent, Streamed,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Answers with an event stream made of the chunks the test sends, after `delay`; `None` ends it.
async fn stream_server(delay: Duration) -> (String, mpsc::UnboundedSender<Option<&'static str>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/events", listener.local_addr().unwrap());
    let (tx, mut chunks) = mpsc::unbounded_channel::<Option<&'static str>>();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.ends_with(b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..read]);
        }
        tokio::time::sleep(delay).await;
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
        if stream.write_all(head.as_bytes()).await.is_err() {
            return;
        }
        while let Some(Some(chunk)) = chunks.recv().await {
            let framed = format!("{:x}\r\n{chunk}\r\n", chunk.len());
            if stream.write_all(framed.as_bytes()).await.is_err() {
                return;
            }
        }
        let _ = stream.write_all(b"0\r\n\r\n").await;
    });
    (url, tx)
}

struct Session {
    updates: mpsc::UnboundedReceiver<Streamed>,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<Response, SendError>>,
}

fn start(options: &ClientOptions, url: &str) -> Session {
    let client = HttpClient::new(options).unwrap();
    let request = Request::new(Method::Get, url);
    let (tx, updates) = mpsc::unbounded_channel();
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let stopped = async {
            let _ = stopped.await;
        };
        client
            .send_streaming(&request, stopped, move |update| {
                let _ = tx.send(update);
            })
            .await
    });
    Session {
        updates,
        stop: Some(stop),
        task,
    }
}

impl Session {
    async fn next(&mut self) -> Streamed {
        tokio::time::timeout(Duration::from_secs(5), self.updates.recv())
            .await
            .unwrap()
            .unwrap()
    }

    async fn event(&mut self) -> SseEvent {
        match self.next().await {
            Streamed::Event(event) => event,
            other => panic!("expected an event, got {other:?}"),
        }
    }

    async fn finish(self) -> Result<Response, SendError> {
        let _keep_running = self.stop;
        tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .unwrap()
            .unwrap()
    }
}

#[tokio::test]
async fn events_arrive_before_the_stream_ends() {
    let (url, server) = stream_server(Duration::ZERO).await;
    let mut session = start(&ClientOptions::default(), &url);
    server.send(Some("data: one\n\n")).unwrap();
    match session.next().await {
        Streamed::Head(head) => {
            assert_eq!(head.status, 200);
            assert!(head.body.is_empty());
        }
        other => panic!("expected the head, got {other:?}"),
    }
    assert_eq!(session.event().await.data, "one");

    server
        .send(Some("event: tick\nid: 2\ndata: {\"n\":2}\n"))
        .unwrap();
    server.send(Some("\n")).unwrap();
    let tick = session.event().await;
    assert_eq!(
        (tick.event.as_str(), tick.data.as_str(), tick.id.as_deref()),
        ("tick", "{\"n\":2}", Some("2"))
    );
    server.send(None).unwrap();
    let response = session.finish().await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(
        response.text(),
        "data: one\n\nevent: tick\nid: 2\ndata: {\"n\":2}\n\n"
    );
}

#[tokio::test]
async fn stop_keeps_what_was_received() {
    let (url, server) = stream_server(Duration::ZERO).await;
    let mut session = start(&ClientOptions::default(), &url);
    server.send(Some("data: one\n\n")).unwrap();
    session.next().await;
    session.event().await;
    session.stop.take().unwrap().send(()).unwrap();
    let response = session.finish().await.unwrap();
    assert_eq!(response.text(), "data: one\n\n");
    drop(server);
}

#[tokio::test]
async fn the_timeout_does_not_cut_an_event_stream() {
    let (url, server) = stream_server(Duration::ZERO).await;
    let options = ClientOptions {
        timeout: Some(Duration::from_millis(300)),
        ..Default::default()
    };
    let mut session = start(&options, &url);
    server.send(Some("data: one\n\n")).unwrap();
    session.next().await;
    session.event().await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    server.send(Some("data: two\n\n")).unwrap();
    assert_eq!(session.event().await.data, "two");
    server.send(None).unwrap();
    assert!(session.finish().await.is_ok());
}

#[tokio::test]
async fn slow_headers_still_time_out() {
    let (url, _server) = stream_server(Duration::from_secs(2)).await;
    let options = ClientOptions {
        timeout: Some(Duration::from_millis(200)),
        ..Default::default()
    };
    let result = start(&options, &url).finish().await;
    assert!(matches!(result, Err(SendError::Timeout)), "{result:?}");
}

#[tokio::test]
async fn other_responses_are_read_whole() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(
            ResponseTemplate::new(201)
                .insert_header("content-type", "application/json")
                .set_body_string("{\"ok\":true}"),
        )
        .mount(&server)
        .await;
    let mut session = start(&ClientOptions::default(), &server.uri());
    let response = (&mut session.task).await.unwrap().unwrap();
    assert_eq!(response.status, 201);
    assert_eq!(response.text(), "{\"ok\":true}");
    assert!(session.updates.try_recv().is_err());
}
