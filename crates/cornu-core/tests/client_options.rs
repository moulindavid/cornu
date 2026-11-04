use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use cornu_core::{ClientOptions, HttpClient, Method, ProxyMode, Request, SendError};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use wiremock::matchers::path;
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Pki {
    ca: String,
    issuer: Issuer<'static, KeyPair>,
}

struct Leaf {
    der: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
    /// Certificate then key, as `client_certificate` expects.
    pem: String,
}

impl Pki {
    fn new() -> Self {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params
            .distinguished_name
            .push(DnType::CommonName, "cornu test CA");
        let ca = params.self_signed(&key).unwrap().pem();
        Self {
            ca,
            issuer: Issuer::new(params, key),
        }
    }

    fn leaf(&self, names: &[&str], purpose: ExtendedKeyUsagePurpose) -> Leaf {
        let key = KeyPair::generate().unwrap();
        let names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
        let mut params = CertificateParams::new(names).unwrap();
        params.extended_key_usages = vec![purpose];
        let cert = params.signed_by(&key, &self.issuer).unwrap();
        Leaf {
            pem: format!("{}{}", cert.pem(), key.serialize_pem()),
            der: cert.der().clone(),
            key: PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        }
    }
}

/// An HTTPS server on localhost answering every request with `200 hello`; with `client_ca`, it
/// only accepts clients holding a certificate signed by it.
async fn https_server(pki: &Pki, client_ca: bool) -> String {
    let leaf = pki.leaf(&["localhost"], ExtendedKeyUsagePurpose::ServerAuth);
    let builder = rustls::ServerConfig::builder();
    let builder = if client_ca {
        let mut roots = rustls::RootCertStore::empty();
        let ca = rustls::pki_types::pem::PemObject::from_pem_slice(pki.ca.as_bytes()).unwrap();
        roots.add(ca).unwrap();
        let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
            .build()
            .unwrap();
        builder.with_client_cert_verifier(verifier)
    } else {
        builder.with_no_client_auth()
    };
    let config = builder.with_single_cert(vec![leaf.der], leaf.key).unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut stream) = acceptor.accept(stream).await else {
                    return;
                };
                let mut buffer = [0; 4096];
                let _ = stream.read(&mut buffer).await;
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
                    )
                    .await;
                let _ = stream.shutdown().await;
            });
        }
    });
    format!("https://localhost:{port}/")
}

async fn get(options: &ClientOptions, url: &str) -> Result<String, SendError> {
    let client = HttpClient::new(options)?;
    let response = client.send(&Request::new(Method::Get, url)).await?;
    Ok(response.text().into_owned())
}

fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    path
}

#[tokio::test]
async fn untrusted_certificates_need_the_ca_or_insecure_mode() {
    let pki = Pki::new();
    let url = https_server(&pki, false).await;
    let dir = tempfile::tempdir().unwrap();

    let error = get(&ClientOptions::default(), &url).await.unwrap_err();
    assert!(
        error.to_string().to_lowercase().contains("certificate"),
        "{error}"
    );

    let insecure = ClientOptions {
        insecure: true,
        ..Default::default()
    };
    assert_eq!(get(&insecure, &url).await.unwrap(), "hello");

    let trusted = ClientOptions {
        ca_certificate: Some(write(dir.path(), "ca.pem", &pki.ca)),
        ..Default::default()
    };
    assert_eq!(get(&trusted, &url).await.unwrap(), "hello");
}

#[tokio::test]
async fn client_certificate_enables_mutual_tls() {
    let pki = Pki::new();
    let url = https_server(&pki, true).await;
    let dir = tempfile::tempdir().unwrap();
    let mut options = ClientOptions {
        ca_certificate: Some(write(dir.path(), "ca.pem", &pki.ca)),
        ..Default::default()
    };
    assert!(get(&options, &url).await.is_err(), "no client certificate");

    let client = pki.leaf(&["cornu"], ExtendedKeyUsagePurpose::ClientAuth);
    options.client_certificate = Some(write(dir.path(), "client.pem", &client.pem));
    assert_eq!(get(&options, &url).await.unwrap(), "hello");
}

#[test]
fn unreadable_certificates_and_proxies_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let build = |options: ClientOptions| HttpClient::new(&options).map(|_| ()).unwrap_err();

    let missing = build(ClientOptions {
        ca_certificate: Some(dir.path().join("nope.pem")),
        ..Default::default()
    });
    assert!(matches!(missing, SendError::ReadFile { .. }), "{missing}");

    let garbage = write(dir.path(), "garbage.pem", "not a certificate");
    let empty = build(ClientOptions {
        ca_certificate: Some(garbage.clone()),
        ..Default::default()
    });
    assert!(empty.to_string().contains("no PEM certificate"), "{empty}");
    let identity = build(ClientOptions {
        client_certificate: Some(garbage),
        ..Default::default()
    });
    assert!(
        matches!(identity, SendError::InvalidCertificate { .. }),
        "{identity}"
    );

    let proxy = build(ClientOptions {
        proxy: ProxyMode::Custom {
            url: "not a url".into(),
            no_proxy: String::new(),
        },
        ..Default::default()
    });
    assert!(matches!(proxy, SendError::InvalidProxy { .. }), "{proxy}");
}

#[tokio::test]
async fn custom_proxy_is_used_except_for_no_proxy_hosts() {
    let proxy = MockServer::start().await;
    Mock::given(path("/hello"))
        .respond_with(ResponseTemplate::new(200).set_body_string("via proxy"))
        .mount(&proxy)
        .await;
    let direct = MockServer::start().await;
    Mock::given(path("/hello"))
        .respond_with(ResponseTemplate::new(200).set_body_string("direct"))
        .mount(&direct)
        .await;

    let mut options = ClientOptions {
        proxy: ProxyMode::Custom {
            url: proxy.uri(),
            no_proxy: String::new(),
        },
        ..Default::default()
    };
    assert_eq!(
        get(&options, "http://api.example.invalid/hello")
            .await
            .unwrap(),
        "via proxy"
    );
    let requests = proxy.received_requests().await.unwrap();
    assert_eq!(requests[0].url.host_str(), Some("api.example.invalid"));

    options.proxy = ProxyMode::Custom {
        url: proxy.uri(),
        no_proxy: "localhost, 127.0.0.1".into(),
    };
    let url = format!("{}/hello", direct.uri());
    assert_eq!(get(&options, &url).await.unwrap(), "direct");
}

#[test]
fn options_are_stored_compactly_with_paths_relative_to_the_workspace() {
    assert_eq!(toml::to_string(&ClientOptions::default()).unwrap(), "");
    let options = ClientOptions {
        timeout: None,
        insecure: true,
        proxy: ProxyMode::Custom {
            url: "http://proxy:3128".into(),
            no_proxy: String::new(),
        },
        client_certificate: Some("certs/client.pem".into()),
        ..Default::default()
    };
    let text = toml::to_string(&options).unwrap();
    assert_eq!(
        text,
        "timeout = 0\ninsecure = true\nclient_certificate = \"certs/client.pem\"\n\n[proxy]\nmode = \"custom\"\nurl = \"http://proxy:3128\"\n"
    );
    assert_eq!(toml::from_str::<ClientOptions>(&text).unwrap(), options);
    assert_eq!(
        toml::from_str::<ClientOptions>("timeout = 5")
            .unwrap()
            .timeout,
        Some(Duration::from_secs(5))
    );

    let resolved = options.relative_to(Path::new("/work"));
    assert_eq!(
        resolved.client_certificate.as_deref(),
        Some(Path::new("/work/certs/client.pem"))
    );
    let absolute = ClientOptions {
        ca_certificate: Some("/etc/ca.pem".into()),
        ..Default::default()
    };
    assert_eq!(absolute.relative_to(Path::new("/work")), absolute);
}
