#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

struct Empty;

impl BodyChunks for Empty {
    fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, TransportError>> {
        Box::pin(async { Ok(None) })
    }
}

#[test]
fn methods_and_headers() {
    assert_eq!(HttpMethod::Get.as_str(), "GET");
    assert_eq!(HttpMethod::Head.as_str(), "HEAD");
    let request = TransportRequest {
        method: HttpMethod::Get,
        url: "https://x.test/".to_owned(),
        pinned: vec!["93.184.216.34:443".parse().unwrap()],
        headers: vec![("User-Agent".to_owned(), "t".to_owned())],
        connect_timeout: Duration::from_secs(1),
    };
    assert_eq!(request.header("user-agent"), Some("t"));
    assert_eq!(request.header("accept"), None);
}

#[tokio::test]
async fn responses_redact_their_url_in_debug() {
    let mut response = TransportResponse::new(
        200,
        "https://x.test/a?token=secret",
        vec![("ETag".to_owned(), "e".to_owned())],
        Box::new(Empty),
    );
    assert_eq!(response.header("etag"), Some("e"));
    let debug = format!("{response:?}");
    assert!(debug.contains("https://x.test/a"));
    assert!(!debug.contains("secret"));
    assert_eq!(response.body.next_chunk().await.unwrap(), None);
}

#[tokio::test]
async fn system_resolver_resolves_literals_without_dns() {
    let addresses = SystemResolver.resolve("127.0.0.1", 8080).await.unwrap();
    assert_eq!(addresses, vec!["127.0.0.1:8080".parse().unwrap()]);
    let shared: Arc<dyn Resolver> = Arc::new(SystemResolver);
    assert_eq!(Arc::new(shared).resolve("::1", 1).await.unwrap().len(), 1);
}

#[test]
fn transport_errors_display() {
    assert_eq!(TransportError::Timeout.to_string(), "transport timed out");
    assert!(TransportError::Io("x".to_owned()).to_string().contains('x'));
}
