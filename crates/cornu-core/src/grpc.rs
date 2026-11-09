use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use http::uri::PathAndQuery;
use hyper_util::rt::TokioIo;
use prost::Message as _;
use prost_reflect::prost_types::FileDescriptorProto;
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor, MethodDescriptor};
use prost_reflect::{SerializeOptions, ServiceDescriptor};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde_json::Value;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::metadata::MetadataMap;
use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Status};
use tonic_reflection::pb::v1::server_reflection_request::MessageRequest;
use tonic_reflection::pb::v1::server_reflection_response::MessageResponse;
use tonic_reflection::pb::v1::{ServerReflectionRequest, ServerReflectionResponse};
use url::Url;

use crate::http::read_pem;
use crate::sse::within;
use crate::{Body, ClientOptions, HttpClient, Request, Response, SendError, SseEvent, Streamed};

/// `grpc://` (plaintext HTTP/2) and `grpcs://` URLs call a gRPC method, named by their path:
/// `grpcs://host:443/package.Service/Method`.
pub fn is_grpc_url(url: &str) -> bool {
    let url = url.trim_start().to_ascii_lowercase();
    url.starts_with("grpc://") || url.starts_with("grpcs://")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrpcMethod {
    pub service: String,
    pub name: String,
    pub client_streaming: bool,
    pub server_streaming: bool,
    /// The input message with every field at its default, as JSON; an array of one message for
    /// client streaming methods.
    pub template: String,
}

impl GrpcMethod {
    pub fn path(&self) -> String {
        format!("{}/{}", self.service, self.name)
    }

    fn new(method: &MethodDescriptor) -> Self {
        let empty = DynamicMessage::new(method.input());
        let options = SerializeOptions::new().skip_default_fields(false);
        let mut template = empty
            .serialize_with_options(serde_json::value::Serializer, &options)
            .unwrap_or(Value::Null);
        if method.is_client_streaming() {
            template = Value::Array(vec![template]);
        }
        Self {
            service: method.parent_service().full_name().to_owned(),
            name: method.name().to_owned(),
            client_streaming: method.is_client_streaming(),
            server_streaming: method.is_server_streaming(),
            template: serde_json::to_string_pretty(&template).unwrap_or_default(),
        }
    }
}

struct Target {
    host: String,
    port: u16,
    tls: bool,
    /// `package.Service/Method`, empty when the URL has no path.
    method: String,
}

fn target(url: &str) -> Result<Target, SendError> {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    let (tls, rest) = if lower.starts_with("grpcs://") {
        (true, &url[8..])
    } else if lower.starts_with("grpc://") {
        (false, &url[7..])
    } else {
        return Err(SendError::Grpc(format!(
            "{url} is not a grpc:// or grpcs:// URL"
        )));
    };
    let parsed = Url::parse(&format!("http://{rest}")).map_err(|source| SendError::InvalidUrl {
        url: url.to_owned(),
        source,
    })?;
    let host = parsed.host_str().unwrap_or_default();
    Ok(Target {
        host: host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned(),
        port: parsed.port().unwrap_or(if tls { 443 } else { 80 }),
        tls,
        method: parsed.path().trim_matches('/').to_owned(),
    })
}

impl HttpClient {
    /// Every method the server lists through reflection.
    pub async fn grpc_methods(&self, request: &Request) -> Result<Vec<GrpcMethod>, SendError> {
        let target = target(&request.url)?;
        let deadline = self.deadline();
        let mut reflection = within(deadline, self.reflection(&target, request)).await??;
        let services = within(deadline, reflection.services()).await??;
        let pool = within(deadline, reflection.pool(&services)).await??;
        Ok(services
            .iter()
            .filter_map(|name| pool.get_service_by_name(name))
            .flat_map(|service| {
                service
                    .methods()
                    .map(|method| GrpcMethod::new(&method))
                    .collect::<Vec<_>>()
            })
            .collect())
    }

    /// Calls the method of the URL with the JSON body, a JSON array for client streaming. Server
    /// streams are reported as they arrive, like event streams, and end when `stop` completes.
    /// The response body holds the output message, or the array of every output message.
    pub async fn grpc_call(
        &self,
        request: &Request,
        stop: impl Future<Output = ()>,
        mut on_update: impl FnMut(Streamed),
    ) -> Result<Response, SendError> {
        let target = target(&request.url)?;
        let Some((service, name)) = target.method.rsplit_once('/') else {
            return Err(SendError::Grpc(
                "pick a method: the URL must end with /package.Service/Method".into(),
            ));
        };
        let deadline = self.deadline();
        let mut reflection = within(deadline, self.reflection(&target, request)).await??;
        let pool = within(deadline, reflection.pool(&[service.to_owned()])).await??;
        let method = find_method(&pool, service, name)?;
        let messages = input_messages(&method, &request.body)?;
        let path = PathAndQuery::try_from(format!("/{service}/{name}"))
            .map_err(|error| SendError::Grpc(error.to_string()))?;

        let started = Instant::now();
        let mut grpc = tonic::client::Grpc::new(reflection.channel.clone());
        let mut call = tonic::Request::new(futures_util::stream::iter(messages));
        *call.metadata_mut() = reflection.metadata.clone();
        let codec = DynamicCodec(method.output());
        let response = within(deadline, async {
            grpc.ready().await.map_err(connect_error)?;
            grpc.streaming(call, path, codec)
                .await
                .map_err(status_error)
        })
        .await??;
        let (metadata, mut stream, _) = response.into_parts();
        let mut headers = metadata_pairs(metadata);
        let streaming = method.is_server_streaming();
        if streaming {
            on_update(Streamed::Head(Response {
                status: 200,
                headers: headers.clone(),
                body: Vec::new(),
                elapsed: started.elapsed(),
            }));
        }

        let mut outputs = Vec::new();
        let mut stopped = false;
        let mut stop = std::pin::pin!(stop);
        loop {
            let next = if streaming {
                tokio::select! {
                    next = stream.message() => next,
                    () = &mut stop => {
                        stopped = true;
                        break;
                    }
                }
            } else {
                within(deadline, stream.message()).await?
            };
            let Some(message) = next.map_err(status_error)? else {
                break;
            };
            let value = message
                .serialize_with_options(serde_json::value::Serializer, &SerializeOptions::new())
                .map_err(|error| SendError::Grpc(error.to_string()))?;
            if streaming {
                on_update(Streamed::Event(SseEvent {
                    event: "message".into(),
                    data: value.to_string(),
                    id: None,
                    retry: None,
                }));
            }
            outputs.push(value);
        }
        if !stopped && let Ok(Some(trailers)) = stream.trailers().await {
            headers.extend(metadata_pairs(trailers));
        }
        let body = if streaming {
            Value::Array(outputs)
        } else {
            outputs.pop().unwrap_or(Value::Null)
        };
        Ok(Response {
            status: 200,
            headers,
            body: serde_json::to_vec_pretty(&body).unwrap_or_default(),
            elapsed: started.elapsed(),
        })
    }

    fn deadline(&self) -> Option<tokio::time::Instant> {
        self.options
            .timeout
            .map(|timeout| tokio::time::Instant::now() + timeout)
    }

    async fn reflection(
        &self,
        target: &Target,
        request: &Request,
    ) -> Result<Reflection, SendError> {
        Ok(Reflection {
            metadata: MetadataMap::from_headers(self.headers(request).await?),
            channel: channel(&self.options, target).await?,
            path: REFLECTION_V1,
        })
    }
}

fn find_method(
    pool: &DescriptorPool,
    service: &str,
    name: &str,
) -> Result<MethodDescriptor, SendError> {
    let service: ServiceDescriptor = pool
        .get_service_by_name(service)
        .ok_or_else(|| SendError::Grpc(format!("the server has no service {service}")))?;
    service
        .methods()
        .find(|method| method.name() == name)
        .ok_or_else(|| SendError::Grpc(format!("{} has no method {name}", service.full_name())))
}

fn input_messages(
    method: &MethodDescriptor,
    body: &Body,
) -> Result<Vec<DynamicMessage>, SendError> {
    let text = match body {
        Body::Json { text } | Body::Raw { text, .. } => text.trim(),
        _ => "",
    };
    let value: Value = serde_json::from_str(if text.is_empty() { "{}" } else { text })
        .map_err(|error| SendError::Grpc(format!("the message is not valid JSON: {error}")))?;
    let values = match value {
        Value::Array(values) if method.is_client_streaming() => values,
        value => vec![value],
    };
    let input = method.input();
    values
        .into_iter()
        .map(|value| {
            DynamicMessage::deserialize(input.clone(), value).map_err(|error| {
                SendError::Grpc(format!(
                    "the message does not match {}: {error}",
                    input.full_name()
                ))
            })
        })
        .collect()
}

fn metadata_pairs(metadata: MetadataMap) -> Vec<(String, String)> {
    metadata
        .into_headers()
        .iter()
        .map(|(name, value)| {
            let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
            (name.as_str().to_owned(), value)
        })
        .collect()
}

fn status_error(status: Status) -> SendError {
    let code = status.code();
    let mut text = format!("{code:?} ({})", code as i32);
    if !status.message().is_empty() {
        text.push_str(": ");
        text.push_str(status.message());
    }
    SendError::Grpc(text)
}

fn connect_error(error: impl std::error::Error) -> SendError {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    SendError::Connect(message)
}

const REFLECTION_V1: &str = "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo";
const REFLECTION_V1ALPHA: &str = "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo";

struct Reflection {
    channel: Channel,
    metadata: MetadataMap,
    path: &'static str,
}

impl Reflection {
    /// Servers implement v1, v1alpha or both, with the same messages.
    async fn ask(&mut self, request: MessageRequest) -> Result<MessageResponse, SendError> {
        loop {
            match self.try_ask(request.clone()).await {
                Ok(MessageResponse::ErrorResponse(error)) => {
                    return Err(SendError::Grpc(format!(
                        "server reflection: {}",
                        error.error_message
                    )));
                }
                Ok(response) => return Ok(response),
                Err(status)
                    if status.code() == Code::Unimplemented && self.path == REFLECTION_V1 =>
                {
                    self.path = REFLECTION_V1ALPHA;
                }
                Err(status) if status.code() == Code::Unimplemented => {
                    return Err(SendError::Grpc(
                        "the server does not offer reflection, which is needed to call it".into(),
                    ));
                }
                Err(status) => return Err(status_error(status)),
            }
        }
    }

    async fn try_ask(&self, request: MessageRequest) -> Result<MessageResponse, Status> {
        let mut grpc = tonic::client::Grpc::new(self.channel.clone());
        grpc.ready()
            .await
            .map_err(|error| Status::unavailable(connect_error(error).to_string()))?;
        let message = ServerReflectionRequest {
            host: String::new(),
            message_request: Some(request),
        };
        let mut call = tonic::Request::new(futures_util::stream::iter([message]));
        *call.metadata_mut() = self.metadata.clone();
        let codec =
            tonic_prost::ProstCodec::<ServerReflectionRequest, ServerReflectionResponse>::default();
        let mut stream = grpc
            .streaming(call, PathAndQuery::from_static(self.path), codec)
            .await?
            .into_inner();
        stream
            .message()
            .await?
            .and_then(|response| response.message_response)
            .ok_or_else(|| Status::internal("empty reflection answer"))
    }

    async fn services(&mut self) -> Result<Vec<String>, SendError> {
        match self
            .ask(MessageRequest::ListServices(String::new()))
            .await?
        {
            MessageResponse::ListServicesResponse(list) => Ok(list
                .service
                .into_iter()
                .map(|service| service.name)
                .filter(|name| !name.starts_with("grpc.reflection."))
                .collect()),
            _ => Err(SendError::Grpc(
                "unexpected answer to the list of services".into(),
            )),
        }
    }

    /// The files defining `symbols` and everything they import.
    async fn pool(&mut self, symbols: &[String]) -> Result<DescriptorPool, SendError> {
        // The well-known types are already there: servers do not always send them.
        let mut pool = DescriptorPool::global();
        let mut files = Vec::new();
        let mut seen = HashSet::new();
        let mut queue: Vec<_> = symbols
            .iter()
            .map(|symbol| MessageRequest::FileContainingSymbol(symbol.clone()))
            .collect();
        while let Some(request) = queue.pop() {
            let MessageResponse::FileDescriptorResponse(response) = self.ask(request).await? else {
                return Err(SendError::Grpc(
                    "unexpected answer to a file request".into(),
                ));
            };
            for bytes in response.file_descriptor_proto {
                let file = FileDescriptorProto::decode(bytes.as_slice())
                    .map_err(|error| SendError::Grpc(format!("invalid descriptor: {error}")))?;
                let name = file.name().to_owned();
                if pool.get_file_by_name(&name).is_some() || !seen.insert(name) {
                    continue;
                }
                for dependency in &file.dependency {
                    if pool.get_file_by_name(dependency).is_none() && !seen.contains(dependency) {
                        queue.push(MessageRequest::FileByFilename(dependency.clone()));
                    }
                }
                files.push(file);
            }
        }
        pool.add_file_descriptor_protos(files)
            .map_err(|error| SendError::Grpc(format!("invalid descriptors: {error}")))?;
        Ok(pool)
    }
}

#[derive(Clone)]
struct DynamicCodec(MessageDescriptor);

impl Codec for DynamicCodec {
    type Encode = DynamicMessage;
    type Decode = DynamicMessage;
    type Encoder = Self;
    type Decoder = Self;

    fn encoder(&mut self) -> Self {
        self.clone()
    }

    fn decoder(&mut self) -> Self {
        self.clone()
    }
}

impl Encoder for DynamicCodec {
    type Item = DynamicMessage;
    type Error = Status;

    fn encode(&mut self, item: DynamicMessage, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        item.encode(dst)
            .map_err(|error| Status::internal(error.to_string()))
    }
}

impl Decoder for DynamicCodec {
    type Item = DynamicMessage;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<DynamicMessage>, Status> {
        DynamicMessage::decode(self.0.clone(), src)
            .map(Some)
            .map_err(|error| Status::internal(error.to_string()))
    }
}

/// Through the same TLS settings as HTTP requests; proxies are not supported.
async fn channel(options: &ClientOptions, target: &Target) -> Result<Channel, SendError> {
    let address = if target.host.contains(':') {
        format!("http://[{}]:{}", target.host, target.port)
    } else {
        format!("http://{}:{}", target.host, target.port)
    };
    let mut endpoint =
        Endpoint::from_shared(address).map_err(|error| SendError::Grpc(error.to_string()))?;
    if let Some(timeout) = options.timeout {
        endpoint = endpoint.connect_timeout(timeout);
    }
    if !target.tls {
        return endpoint.connect().await.map_err(connect_error);
    }
    let connector = tokio_rustls::TlsConnector::from(tls_config(options)?);
    let (host, port) = (target.host.clone(), target.port);
    endpoint
        .connect_with_connector(tower::service_fn(move |_: http::Uri| {
            let (connector, host) = (connector.clone(), host.clone());
            async move {
                let tcp = tokio::net::TcpStream::connect((host.as_str(), port)).await?;
                let name = ServerName::try_from(host).map_err(|error| {
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, error)
                })?;
                let tls = connector.connect(name, tcp).await?;
                Ok::<_, std::io::Error>(TokioIo::new(tls))
            }
        }))
        .await
        .map_err(connect_error)
}

fn tls_config(options: &ClientOptions) -> Result<Arc<rustls::ClientConfig>, SendError> {
    let tls_error = |error: rustls::Error| SendError::Connect(format!("TLS: {error}"));
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(tls_error)?;
    let verifier: Arc<dyn ServerCertVerifier> = if options.insecure {
        Arc::new(AnyCertificate(provider.clone()))
    } else {
        let mut roots = Vec::new();
        if let Some(path) = &options.ca_certificate {
            for certificate in CertificateDer::pem_slice_iter(&read_pem(path)?) {
                roots.push(certificate.map_err(|error| SendError::InvalidCertificate {
                    path: path.clone(),
                    message: error.to_string(),
                })?);
            }
        }
        Arc::new(
            rustls_platform_verifier::Verifier::new_with_extra_roots(roots, provider)
                .map_err(tls_error)?,
        )
    };
    let builder = builder
        .dangerous()
        .with_custom_certificate_verifier(verifier);
    let mut config = match &options.client_certificate {
        Some(path) => {
            let pem = read_pem(path)?;
            let invalid = |message: String| SendError::InvalidCertificate {
                path: path.clone(),
                message,
            };
            let certificates = CertificateDer::pem_slice_iter(&pem)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| invalid(error.to_string()))?;
            let key =
                PrivateKeyDer::from_pem_slice(&pem).map_err(|error| invalid(error.to_string()))?;
            builder
                .with_client_auth_cert(certificates, key)
                .map_err(|error| invalid(error.to_string()))?
        }
        None => builder.with_no_client_auth(),
    };
    config.alpn_protocols = vec![b"h2".to_vec()];
    Ok(Arc::new(config))
}

#[derive(Debug)]
struct AnyCertificate(Arc<CryptoProvider>);

impl ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_name_the_server_and_the_method() {
        assert!(is_grpc_url("grpc://localhost:50051"));
        assert!(is_grpc_url(" GRPCS://api.example"));
        assert!(!is_grpc_url("https://api.example"));

        let plain = target("grpc://localhost:50051/pkg.Greeter/SayHello").unwrap();
        assert_eq!(
            (
                plain.host.as_str(),
                plain.port,
                plain.tls,
                plain.method.as_str()
            ),
            ("localhost", 50051, false, "pkg.Greeter/SayHello")
        );
        let secure = target("grpcs://[::1]").unwrap();
        assert_eq!(
            (
                secure.host.as_str(),
                secure.port,
                secure.tls,
                secure.method.as_str()
            ),
            ("::1", 443, true, "")
        );
        assert!(target("https://api.example").is_err());
    }
}
