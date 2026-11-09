use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use cornu_core::{
    Auth, Body, ClientOptions, GrpcMethod, HttpClient, Method, Request, Response, SendError,
    Streamed,
};
use futures_util::StreamExt;
use prost_types::field_descriptor_proto::{Label, Type};
use prost_types::{
    DescriptorProto, FieldDescriptorProto, FileDescriptorProto, FileDescriptorSet,
    MethodDescriptorProto, ServiceDescriptorProto,
};
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tonic::server::NamedService;
use tonic::{Status, Streaming};
use tonic_prost::ProstCodec;

#[derive(Clone, PartialEq, prost::Message)]
struct HelloRequest {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(message, optional, tag = "2")]
    lucky: Option<Number>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct HelloReply {
    #[prost(string, tag = "1")]
    message: String,
}

#[derive(Clone, PartialEq, prost::Message)]
struct Number {
    #[prost(int32, tag = "1")]
    n: i32,
}

fn field(name: &str, number: i32, kind: Type, type_name: Option<&str>) -> FieldDescriptorProto {
    FieldDescriptorProto {
        name: Some(name.into()),
        json_name: Some(name.into()),
        number: Some(number),
        label: Some(Label::Optional as i32),
        r#type: Some(kind as i32),
        type_name: type_name.map(Into::into),
        ..Default::default()
    }
}

fn message(name: &str, fields: Vec<FieldDescriptorProto>) -> DescriptorProto {
    DescriptorProto {
        name: Some(name.into()),
        field: fields,
        ..Default::default()
    }
}

fn method(
    name: &str,
    input: &str,
    output: &str,
    client: bool,
    server: bool,
) -> MethodDescriptorProto {
    MethodDescriptorProto {
        name: Some(name.into()),
        input_type: Some(format!(".test.{input}")),
        output_type: Some(format!(".test.{output}")),
        client_streaming: Some(client),
        server_streaming: Some(server),
        ..Default::default()
    }
}

/// What `protoc` would produce for the `Greeter` below.
fn descriptors() -> FileDescriptorSet {
    FileDescriptorSet {
        file: vec![FileDescriptorProto {
            name: Some("test/greeter.proto".into()),
            package: Some("test".into()),
            syntax: Some("proto3".into()),
            message_type: vec![
                message(
                    "HelloRequest",
                    vec![
                        field("name", 1, Type::String, None),
                        field("lucky", 2, Type::Message, Some(".test.Number")),
                    ],
                ),
                message("HelloReply", vec![field("message", 1, Type::String, None)]),
                message("Number", vec![field("n", 1, Type::Int32, None)]),
            ],
            service: vec![ServiceDescriptorProto {
                name: Some("Greeter".into()),
                method: vec![
                    method("SayHello", "HelloRequest", "HelloReply", false, false),
                    method("Count", "Number", "Number", false, true),
                    method("Sum", "Number", "Number", true, false),
                    method("Fail", "HelloRequest", "HelloReply", false, false),
                ],
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
}

#[derive(Clone)]
struct Greeter;

impl NamedService for Greeter {
    const NAME: &'static str = "test.Greeter";
}

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

impl tower::Service<http::Request<tonic::body::Body>> for Greeter {
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = BoxFuture<Result<Self::Response, Infallible>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<tonic::body::Body>) -> Self::Future {
        Box::pin(async move {
            Ok(match request.uri().path() {
                "/test.Greeter/SayHello" => {
                    let service =
                        tower::service_fn(|request: tonic::Request<HelloRequest>| async move {
                            let auth = request
                                .metadata()
                                .get("authorization")
                                .and_then(|value| value.to_str().ok())
                                .unwrap_or("nobody")
                                .to_owned();
                            let hello = request.into_inner();
                            let lucky = hello.lucky.map_or(0, |lucky| lucky.n);
                            let mut response = tonic::Response::new(HelloReply {
                                message: format!("Hello {} ({lucky}) from {auth}", hello.name),
                            });
                            response
                                .metadata_mut()
                                .insert("x-served-by", "greeter".parse().unwrap());
                            Ok::<_, Status>(response)
                        });
                    tonic::server::Grpc::new(ProstCodec::default())
                        .unary(service, request)
                        .await
                }
                "/test.Greeter/Count" => {
                    let service = tower::service_fn(|request: tonic::Request<Number>| async move {
                        let to = request.into_inner().n;
                        let stream = futures_util::stream::iter(1..=to).then(|n| async move {
                            if n > 1 {
                                tokio::time::sleep(Duration::from_millis(200)).await;
                            }
                            Ok::<_, Status>(Number { n })
                        });
                        Ok::<_, Status>(tonic::Response::new(Box::pin(stream)))
                    });
                    tonic::server::Grpc::new(ProstCodec::default())
                        .server_streaming(service, request)
                        .await
                }
                "/test.Greeter/Sum" => {
                    let service = tower::service_fn(
                        |request: tonic::Request<Streaming<Number>>| async move {
                            let mut numbers = request.into_inner();
                            let mut n = 0;
                            while let Some(number) = numbers.message().await? {
                                n += number.n;
                            }
                            Ok::<_, Status>(tonic::Response::new(Number { n }))
                        },
                    );
                    tonic::server::Grpc::new(ProstCodec::default())
                        .client_streaming(service, request)
                        .await
                }
                "/test.Greeter/Fail" => {
                    let service = tower::service_fn(|_: tonic::Request<HelloRequest>| async {
                        Err::<tonic::Response<HelloReply>, _>(Status::not_found("nobody here"))
                    });
                    tonic::server::Grpc::new(ProstCodec::default())
                        .unary(service, request)
                        .await
                }
                _ => Status::unimplemented("").into_http(),
            })
        })
    }
}

#[derive(Clone, Copy)]
enum Reflection {
    V1,
    V1Alpha,
}

fn router(reflection: Reflection) -> tonic::transport::server::Router {
    let builder =
        tonic_reflection::server::Builder::configure().register_file_descriptor_set(descriptors());
    let server = tonic::transport::Server::builder();
    let mut server = server;
    match reflection {
        Reflection::V1 => server.add_service(builder.build_v1().unwrap()),
        Reflection::V1Alpha => server.add_service(builder.build_v1alpha().unwrap()),
    }
    .add_service(Greeter)
}

async fn plain_server(reflection: Reflection) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let incoming = tonic::transport::server::TcpIncoming::from(listener);
    tokio::spawn(router(reflection).serve_with_incoming(incoming));
    format!("grpc://{address}")
}

struct Tls(tokio_rustls::server::TlsStream<TcpStream>);

impl tonic::transport::server::Connected for Tls {
    type ConnectInfo = ();

    fn connect_info(&self) {}
}

impl AsyncRead for Tls {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for Tls {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

/// Returns the URL and the PEM of the CA that signed the server certificate.
async fn tls_server() -> (String, String) {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_pem = ca_params.self_signed(&ca_key).unwrap().pem();
    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let key = KeyPair::generate().unwrap();
    let cert = CertificateParams::new(vec!["localhost".to_owned()])
        .unwrap()
        .signed_by(&key, &issuer)
        .unwrap();
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![cert.der().clone()],
        rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
    )
    .unwrap();
    config.alpn_protocols = vec![b"h2".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::unbounded_channel::<Result<Tls, std::io::Error>>();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (acceptor, tx) = (acceptor.clone(), tx.clone());
            tokio::spawn(async move {
                if let Ok(stream) = acceptor.accept(stream).await {
                    let _ = tx.send(Ok(Tls(stream)));
                }
            });
        }
    });
    let incoming = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    tokio::spawn(router(Reflection::V1).serve_with_incoming(incoming));
    (format!("grpcs://localhost:{port}"), ca_pem)
}

fn json(text: &str) -> Body {
    Body::Json { text: text.into() }
}

async fn call(
    client: &HttpClient,
    request: &Request,
) -> (Result<Response, SendError>, Vec<Streamed>) {
    let mut updates = Vec::new();
    let result = client
        .grpc_call(request, std::future::pending(), |update| {
            updates.push(update)
        })
        .await;
    (result, updates)
}

#[tokio::test]
async fn methods_are_listed_through_reflection_v1_and_v1alpha() {
    for reflection in [Reflection::V1, Reflection::V1Alpha] {
        let url = plain_server(reflection).await;
        let client = HttpClient::new(&ClientOptions::default()).unwrap();
        let methods = client
            .grpc_methods(&Request::new(Method::Post, url))
            .await
            .unwrap();
        let summary: Vec<_> = methods
            .iter()
            .map(|method| {
                (
                    method.path(),
                    method.client_streaming,
                    method.server_streaming,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("test.Greeter/SayHello".to_owned(), false, false),
                ("test.Greeter/Count".to_owned(), false, true),
                ("test.Greeter/Sum".to_owned(), true, false),
                ("test.Greeter/Fail".to_owned(), false, false),
            ]
        );
        let hello: &GrpcMethod = &methods[0];
        let template: serde_json::Value = serde_json::from_str(&hello.template).unwrap();
        assert_eq!(template["name"], "");
        let sum: serde_json::Value = serde_json::from_str(&methods[2].template).unwrap();
        assert!(sum.is_array(), "{sum}");
    }
}

#[tokio::test]
async fn unary_calls_send_json_metadata_and_auth() {
    let url = plain_server(Reflection::V1).await;
    let client = HttpClient::new(&ClientOptions::default()).unwrap();
    let request = Request::new(Method::Post, format!("{url}/test.Greeter/SayHello"))
        .with_auth(Auth::Bearer {
            token: "abc".into(),
        })
        .with_body(json(r#"{"name": "Ada", "lucky": {"n": 7}}"#));
    let (result, updates) = call(&client, &request).await;
    let response = result.unwrap();
    assert!(updates.is_empty());
    let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(body["message"], "Hello Ada (7) from Bearer abc");
    assert_eq!(response.header("x-served-by"), Some("greeter"));
    assert_eq!(response.header("grpc-status"), Some("0"));
}

#[tokio::test]
async fn server_streams_are_reported_and_can_be_stopped() {
    let url = plain_server(Reflection::V1).await;
    let client = HttpClient::new(&ClientOptions {
        timeout: Some(Duration::from_millis(300)),
        ..Default::default()
    })
    .unwrap();
    let request = Request::new(Method::Post, format!("{url}/test.Greeter/Count"))
        .with_body(json(r#"{"n": 3}"#));
    let (result, updates) = call(&client, &request).await;
    let body: serde_json::Value = serde_json::from_slice(&result.unwrap().body).unwrap();
    assert_eq!(body, serde_json::json!([{"n": 1}, {"n": 2}, {"n": 3}]));
    assert!(matches!(updates[0], Streamed::Head(_)));
    let events: Vec<_> = updates[1..]
        .iter()
        .map(|update| match update {
            Streamed::Event(event) => event.data.clone(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(events, [r#"{"n":1}"#, r#"{"n":2}"#, r#"{"n":3}"#]);

    let request = request.with_body(json(r#"{"n": 100}"#));
    let (stop, stopped) = oneshot::channel::<()>();
    let mut stop = Some(stop);
    let response = client
        .grpc_call(
            &request,
            async {
                let _ = stopped.await;
            },
            |update| {
                if matches!(update, Streamed::Event(_)) {
                    let _ = stop.take().map(|stop| stop.send(()));
                }
            },
        )
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(body, serde_json::json!([{"n": 1}]));
}

#[tokio::test]
async fn client_streams_take_a_json_array() {
    let url = plain_server(Reflection::V1).await;
    let client = HttpClient::new(&ClientOptions::default()).unwrap();
    let request = Request::new(Method::Post, format!("{url}/test.Greeter/Sum"))
        .with_body(json(r#"[{"n": 1}, {"n": 2}, {"n": 39}]"#));
    let body: serde_json::Value =
        serde_json::from_slice(&call(&client, &request).await.0.unwrap().body).unwrap();
    assert_eq!(body, serde_json::json!({"n": 42}));
}

#[tokio::test]
async fn errors_are_explained() {
    let url = plain_server(Reflection::V1).await;
    let client = HttpClient::new(&ClientOptions::default()).unwrap();
    let error = |path: &str, body: &str| {
        let request = Request::new(Method::Post, format!("{url}{path}")).with_body(json(body));
        let client = client.clone();
        async move { call(&client, &request).await.0.unwrap_err().to_string() }
    };
    for (path, body, expected) in [
        ("/test.Greeter/Fail", "{}", "NotFound (5): nobody here"),
        ("", "{}", "pick a method"),
        ("/test.Nobody/Call", "{}", "'test.Nobody' not found"),
        ("/test.Greeter/Nope", "{}", "has no method Nope"),
        ("/test.Greeter/SayHello", "{", "not valid JSON"),
        (
            "/test.Greeter/SayHello",
            r#"{"age": 3}"#,
            "does not match test.HelloRequest",
        ),
    ] {
        let message = error(path, body).await;
        assert!(message.contains(expected), "{path}: {message}");
    }
}

#[tokio::test]
async fn tls_uses_the_client_settings() {
    let (url, ca) = tls_server().await;
    let request = Request::new(Method::Post, format!("{url}/test.Greeter/SayHello"))
        .with_body(json(r#"{"name": "Ada"}"#));

    let client = HttpClient::new(&ClientOptions::default()).unwrap();
    assert!(
        call(&client, &request).await.0.is_err(),
        "untrusted certificate"
    );

    let insecure = HttpClient::new(&ClientOptions {
        insecure: true,
        ..Default::default()
    })
    .unwrap();
    assert!(call(&insecure, &request).await.0.is_ok());

    let dir = tempfile::tempdir().unwrap();
    let ca_path = dir.path().join("ca.pem");
    std::fs::write(&ca_path, ca).unwrap();
    let trusted = HttpClient::new(&ClientOptions {
        ca_certificate: Some(ca_path),
        ..Default::default()
    })
    .unwrap();
    let response = call(&trusted, &request).await.0.unwrap();
    assert!(response.text().contains("Hello Ada"));
}
