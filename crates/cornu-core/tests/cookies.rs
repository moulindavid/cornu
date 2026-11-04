use cornu_core::{ClientOptions, CookieJar, HttpClient, Method, Request};
use url::Url;
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/login"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Set-Cookie", "session=abc; Path=/; HttpOnly")
                .insert_header("Location", "/home"),
        )
        .mount(&server)
        .await;
    Mock::given(path("/home"))
        .respond_with(ResponseTemplate::new(200).insert_header("Set-Cookie", "theme=dark"))
        .mount(&server)
        .await;
    Mock::given(path("/me"))
        .and(header("cookie", "session=abc; theme=dark"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(path("/me"))
        .and(header("cookie", "mine=1"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;
    Mock::given(path("/me"))
        .and(header_exists("cookie"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&server)
        .await;
    Mock::given(path("/me"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn cookies_from_responses_and_redirects_are_sent_back() {
    let server = server().await;
    let jar = CookieJar::default();
    let client = HttpClient::with_cookies(&ClientOptions::default(), jar.clone()).unwrap();
    let url = |p: &str| format!("{}{p}", server.uri());

    let before = jar.revision();
    let login = client
        .send(&Request::new(Method::Post, url("/login")))
        .await
        .unwrap();
    assert_eq!(login.status, 200, "redirect followed");
    assert!(jar.revision() > before);

    let me = client
        .send(&Request::new(Method::Get, url("/me")))
        .await
        .unwrap();
    assert_eq!(me.status, 200);

    let own = Request::new(Method::Get, url("/me")).with_header("Cookie", "mine=1");
    assert_eq!(
        client.send(&own).await.unwrap().status,
        201,
        "own header kept"
    );

    let names: Vec<_> = jar
        .list()
        .into_iter()
        .map(|c| (c.name, c.http_only))
        .collect();
    assert_eq!(names, [("session".into(), true), ("theme".into(), false)]);

    jar.clear();
    assert!(jar.is_empty());
    let me = client
        .send(&Request::new(Method::Get, url("/me")))
        .await
        .unwrap();
    assert_eq!(me.status, 401);
}

#[tokio::test]
async fn clients_without_a_jar_keep_no_cookies() {
    let server = server().await;
    let client = HttpClient::new(&ClientOptions::default()).unwrap();
    let url = |p: &str| format!("{}{p}", server.uri());
    client
        .send(&Request::new(Method::Post, url("/login")))
        .await
        .unwrap();
    let me = client
        .send(&Request::new(Method::Get, url("/me")))
        .await
        .unwrap();
    assert_eq!(me.status, 401);
}

#[test]
fn jar_matches_domains_and_paths() {
    let jar = CookieJar::default();
    let api = Url::parse("https://api.example.com/v1/users").unwrap();
    assert!(jar.insert("a=1; Domain=example.com; Path=/", &api));
    assert!(jar.insert("b=2; Path=/v1", &api));
    assert!(jar.insert("c=3; Secure", &api));
    assert!(!jar.insert("not a cookie", &api));

    assert_eq!(jar.header_for(&api).as_deref(), Some("b=2; c=3; a=1"));
    let www = Url::parse("http://www.example.com/v1").unwrap();
    assert_eq!(jar.header_for(&www).as_deref(), Some("a=1"));
    let other = Url::parse("https://other.org/").unwrap();
    assert_eq!(jar.header_for(&other), None);

    let cookie = jar.list().into_iter().find(|c| c.name == "b").unwrap();
    assert_eq!(
        (cookie.domain.as_str(), cookie.path.as_str()),
        ("api.example.com", "/v1")
    );
    jar.remove(&cookie.domain, &cookie.path, &cookie.name);
    assert_eq!(jar.header_for(&api).as_deref(), Some("c=3; a=1"));
}

#[test]
fn jar_round_trips_through_json_without_expired_cookies() {
    let jar = CookieJar::default();
    let url = Url::parse("https://example.com/").unwrap();
    jar.insert("session=abc", &url);
    jar.insert("kept=1; Expires=Tue, 03 Aug 2100 00:38:37 GMT", &url);
    jar.insert("gone=1; Expires=Thu, 01 Jan 2015 00:00:00 GMT", &url);

    let restored = CookieJar::default();
    restored.load_json(&jar.to_json().unwrap()).unwrap();
    let cookies = restored.list();
    let names: Vec<_> = cookies.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["kept", "session"]);
    assert_eq!(cookies[0].expires, Some(4_120_936_717));
    assert_eq!(cookies[1].expires, None);
    assert!(restored.load_json("not json").is_err());
}
