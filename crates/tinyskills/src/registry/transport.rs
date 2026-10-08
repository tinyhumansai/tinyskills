//! The host-supplied network boundary: one HTTP exchange and one DNS lookup.

use std::fmt;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// A boxed, `Send` future, the return type of every host-implemented trait
/// method in this module.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The HTTP method of a [`TransportRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HttpMethod {
    /// `GET`.
    Get,
    /// `HEAD`: the transport must not read a body.
    Head,
}

impl HttpMethod {
    /// The method token, `GET` or `HEAD`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
        }
    }
}

/// One HTTP request the registry asks the host to perform.
///
/// The transport contract:
///
/// - Connect only to an address in [`pinned`](Self::pinned), with the host of
///   [`url`](Self::url) as the TLS server name and `Host` header. The guard has
///   already resolved and checked those addresses; resolving again would let
///   a rebinding DNS answer reach a private address.
/// - Do not follow redirects. Return the `3xx` response as is; the guard
///   validates the next hop and sends a new request.
/// - Report the URL that was actually requested as
///   [`TransportResponse::final_url`]. A response for any other URL is
///   rejected as [`RegistryError::TransportContract`](crate::RegistryError::TransportContract).
/// - Send every header in [`headers`](Self::headers) and apply
///   [`connect_timeout`](Self::connect_timeout) to connection setup. The guard
///   bounds the whole exchange separately.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TransportRequest {
    /// The request method.
    pub method: HttpMethod,
    /// The absolute request URL.
    pub url: String,
    /// The checked addresses to connect to, in preference order. Never empty.
    pub pinned: Vec<SocketAddr>,
    /// Request headers, name and value.
    pub headers: Vec<(String, String)>,
    /// Budget for establishing the connection.
    pub connect_timeout: Duration,
}

impl TransportRequest {
    /// The value of the first header named `name`, compared ASCII
    /// case-insensitively.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }
}

/// A streamed response body.
pub trait BodyChunks: Send {
    /// The next chunk of the body, or `None` once it is complete.
    ///
    /// The registry stops reading, and drops the body, as soon as a size limit
    /// is passed.
    fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, TransportError>>;
}

/// The host's answer to a [`TransportRequest`].
#[non_exhaustive]
pub struct TransportResponse {
    /// HTTP status code.
    pub status: u16,
    /// The URL this response answers; must equal the request URL.
    pub final_url: String,
    /// Response headers, name and value.
    pub headers: Vec<(String, String)>,
    /// The body, read lazily.
    pub body: Box<dyn BodyChunks>,
}

impl TransportResponse {
    /// A response with the given status, URL, headers and body.
    #[must_use]
    pub fn new(
        status: u16,
        final_url: impl Into<String>,
        headers: Vec<(String, String)>,
        body: Box<dyn BodyChunks>,
    ) -> Self {
        Self {
            status,
            final_url: final_url.into(),
            headers,
            body,
        }
    }

    /// The value of the first header named `name`, compared ASCII
    /// case-insensitively.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }
}

impl fmt::Debug for TransportResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransportResponse")
            .field("status", &self.status)
            .field("final_url", &crate::redact_url(&self.final_url))
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

/// Why a transport could not complete an exchange.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TransportError {
    /// The connection or the exchange timed out.
    #[error("transport timed out")]
    Timeout,
    /// No connection could be established.
    #[error("connect failed: {0}")]
    Connect(String),
    /// The exchange failed after connecting.
    #[error("transport i/o failed: {0}")]
    Io(String),
}

/// Performs one HTTP exchange for the registry. See [`TransportRequest`] for
/// the contract an implementation must keep.
pub trait RegistryTransport: Send + Sync {
    /// Send `request` and return the response head with a lazily read body.
    fn send(
        &self,
        request: TransportRequest,
    ) -> BoxFuture<'_, Result<TransportResponse, TransportError>>;
}

impl<T: RegistryTransport + ?Sized> RegistryTransport for Arc<T> {
    fn send(
        &self,
        request: TransportRequest,
    ) -> BoxFuture<'_, Result<TransportResponse, TransportError>> {
        (**self).send(request)
    }
}

/// Resolves a host name to socket addresses.
pub trait Resolver: Send + Sync {
    /// Every address `host` resolves to, with `port` applied.
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> BoxFuture<'a, std::io::Result<Vec<SocketAddr>>>;
}

impl<T: Resolver + ?Sized> Resolver for Arc<T> {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> BoxFuture<'a, std::io::Result<Vec<SocketAddr>>> {
        (**self).resolve(host, port)
    }
}

/// The operating system resolver, through `tokio::net::lookup_host`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemResolver;

impl Resolver for SystemResolver {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> BoxFuture<'a, std::io::Result<Vec<SocketAddr>>> {
        Box::pin(async move {
            Ok(tokio::net::lookup_host((host, port))
                .await?
                .collect::<Vec<_>>())
        })
    }
}

pub(crate) fn find_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
