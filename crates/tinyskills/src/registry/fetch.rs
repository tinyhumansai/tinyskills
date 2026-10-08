//! The SSRF guard around the host transport, and the document pipeline.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use super::contract::SkillSummary;
use super::error::RegistryError;
use super::transport::{
    HttpMethod, RegistryTransport, Resolver, TransportError, TransportRequest, find_header,
};
use super::url::normalize_registry_document_url;
use crate::install::{is_non_global_v4, is_non_global_v6};
use crate::{
    DocumentError, FetchedDocument, FlatError, FlatSkill, InstallError, MAX_INSTALL_DOCUMENT_BYTES,
    ScanReport, document_digest, is_loopback_http_url, parse_flat, redact_url, scan_skill,
    validate_fetched_document, validate_install_url,
};

const MIB: u64 = 1024 * 1024;

/// Time budgets for registry operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RegistryTimeouts {
    /// Connection setup, passed to the transport. Default 5 s.
    pub connect: Duration,
    /// A whole catalog download. Default 180 s.
    pub catalog: Duration,
    /// A whole `SKILL.md` download. Default 15 s.
    pub document: Duration,
    /// All candidate-location probes for one entry. Default 15 s.
    pub probe: Duration,
    /// A repository tree listing. Default 20 s.
    pub tree_listing: Duration,
    /// Minimum wait after a failed refresh before the next. Default 60 s.
    pub cooldown: Duration,
}

impl Default for RegistryTimeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(5),
            catalog: Duration::from_secs(180),
            document: Duration::from_secs(15),
            probe: Duration::from_secs(15),
            tree_listing: Duration::from_secs(20),
            cooldown: Duration::from_secs(60),
        }
    }
}

/// Size and count limits for registry operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RegistryLimits {
    /// Bytes of one catalog body. Default 128 MiB.
    pub max_catalog_bytes: u64,
    /// Entries in one catalog. Default 250 000.
    pub max_entries: usize,
    /// Bytes of one `SKILL.md`. Default [`MAX_INSTALL_DOCUMENT_BYTES`].
    pub max_document_bytes: u64,
    /// Bytes of one repository tree listing. Default 8 MiB.
    pub max_tree_listing_bytes: u64,
    /// Redirect hops followed per request. Default 5.
    pub max_redirects: usize,
    /// Largest page a query may ask for. Default 100.
    pub max_page_size: usize,
}

impl Default for RegistryLimits {
    fn default() -> Self {
        Self {
            max_catalog_bytes: 128 * MIB,
            max_entries: 250_000,
            max_document_bytes: MAX_INSTALL_DOCUMENT_BYTES as u64,
            max_tree_listing_bytes: 8 * MIB,
            max_redirects: 5,
            max_page_size: 100,
        }
    }
}

/// Network policy for the request guard.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FetchPolicy {
    /// Allow plain `http` to `localhost`, `127.0.0.1` or `::1`, for tests and
    /// local mirrors. Off by default.
    pub allow_loopback_http: bool,
    /// The `User-Agent` header sent with every request.
    pub user_agent: String,
}

impl Default for FetchPolicy {
    fn default() -> Self {
        Self {
            allow_loopback_http: false,
            user_agent: concat!("tinyskills/", env!("CARGO_PKG_VERSION")).to_owned(),
        }
    }
}

/// A response that passed the guard. Redirects are already followed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GuardedResponse {
    /// HTTP status code.
    pub status: u16,
    /// The URL of the final hop.
    pub url: String,
    /// Response headers.
    pub headers: Vec<(String, String)>,
    /// The body; read only for a `2xx` answer to a `GET`.
    pub body: Vec<u8>,
}

impl GuardedResponse {
    /// The value of the first header named `name`, compared ASCII
    /// case-insensitively.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }

    /// Whether the status is `2xx`.
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// `Retry-After` in its delta-seconds form.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        self.header("retry-after")?
            .trim()
            .parse::<u64>()
            .ok()
            .map(Duration::from_secs)
    }

    /// The error for a non-`2xx` status: [`RegistryError::RateLimited`] for
    /// `429`, or for a `5xx` with `Retry-After`, otherwise
    /// [`RegistryError::Unavailable`].
    #[must_use]
    pub fn status_error(&self) -> RegistryError {
        let retry_after = self.retry_after();
        if self.status == 429 || ((500..600).contains(&self.status) && retry_after.is_some()) {
            RegistryError::RateLimited { retry_after }
        } else {
            RegistryError::Unavailable {
                status: self.status,
            }
        }
    }

    /// `self` when the status is `2xx`.
    ///
    /// # Errors
    ///
    /// Returns [`status_error`](Self::status_error) otherwise.
    pub fn error_for_status(self) -> Result<Self, RegistryError> {
        if self.is_success() {
            Ok(self)
        } else {
            Err(self.status_error())
        }
    }
}

#[derive(Clone)]
pub(crate) struct GuardedFetcher {
    transport: Arc<dyn RegistryTransport>,
    resolver: Arc<dyn Resolver>,
    policy: FetchPolicy,
    connect_timeout: Duration,
    max_redirects: usize,
}

impl fmt::Debug for GuardedFetcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuardedFetcher")
            .field("policy", &self.policy)
            .field("connect_timeout", &self.connect_timeout)
            .field("max_redirects", &self.max_redirects)
            .finish_non_exhaustive()
    }
}

pub(crate) struct FetchSpec<'a> {
    pub(crate) method: HttpMethod,
    pub(crate) url: &'a str,
    pub(crate) headers: &'a [(String, String)],
    pub(crate) max_bytes: u64,
    pub(crate) what: &'static str,
    pub(crate) budget: Duration,
}

impl GuardedFetcher {
    pub(crate) fn new(
        transport: Arc<dyn RegistryTransport>,
        resolver: Arc<dyn Resolver>,
        policy: FetchPolicy,
        timeouts: &RegistryTimeouts,
        limits: &RegistryLimits,
    ) -> Self {
        Self {
            transport,
            resolver,
            policy,
            connect_timeout: timeouts.connect,
            max_redirects: limits.max_redirects,
        }
    }

    pub(crate) async fn fetch(
        &self,
        spec: FetchSpec<'_>,
    ) -> Result<GuardedResponse, RegistryError> {
        let budget = spec.budget;
        let operation = spec.what;
        match tokio::time::timeout(budget, self.fetch_unbounded(spec)).await {
            Ok(Err(RegistryError::Transport(TransportError::Timeout))) | Err(_) => {
                Err(RegistryError::Timeout { operation, budget })
            }
            Ok(result) => result,
        }
    }

    async fn fetch_unbounded(&self, spec: FetchSpec<'_>) -> Result<GuardedResponse, RegistryError> {
        let mut url = spec.url.trim().to_owned();
        let mut headers = spec.headers.to_vec();
        if find_header(&headers, "user-agent").is_none() {
            headers.push(("User-Agent".to_owned(), self.policy.user_agent.clone()));
        }
        let mut hops = 0;
        loop {
            let pinned = self.check_url(&url).await?;
            let request = TransportRequest {
                method: spec.method,
                url: url.clone(),
                pinned,
                headers: headers.clone(),
                connect_timeout: self.connect_timeout,
            };
            let mut response = self
                .transport
                .send(request)
                .await
                .map_err(RegistryError::Transport)?;
            if response.final_url != url {
                return Err(RegistryError::TransportContract {
                    detail: format!(
                        "asked for {} but got a response for {}",
                        redact_url(&url),
                        redact_url(&response.final_url)
                    ),
                });
            }
            if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
                let location =
                    response
                        .header("location")
                        .ok_or_else(|| RegistryError::Malformed {
                            what: "redirect",
                            detail: format!("status {} without a Location header", response.status),
                        })?;
                if hops >= self.max_redirects {
                    return Err(RegistryError::TooLarge {
                        what: "redirect chain",
                        limit: self.max_redirects as u64,
                    });
                }
                let next = next_hop(&url, location)?;
                if !same_origin(&url, &next) {
                    headers.retain(|(name, _)| !is_credential_header(name));
                }
                url = next;
                hops += 1;
                continue;
            }
            let status = response.status;
            let mut body = Vec::new();
            if spec.method == HttpMethod::Get && (200..300).contains(&status) {
                if let Some(length) = response
                    .header("content-length")
                    .and_then(|value| value.trim().parse::<u64>().ok())
                    && length > spec.max_bytes
                {
                    return Err(RegistryError::TooLarge {
                        what: spec.what,
                        limit: spec.max_bytes,
                    });
                }
                while let Some(chunk) = response
                    .body
                    .next_chunk()
                    .await
                    .map_err(RegistryError::Transport)?
                {
                    if (body.len() + chunk.len()) as u64 > spec.max_bytes {
                        return Err(RegistryError::TooLarge {
                            what: spec.what,
                            limit: spec.max_bytes,
                        });
                    }
                    body.extend_from_slice(&chunk);
                }
            }
            return Ok(GuardedResponse {
                status,
                url,
                headers: response.headers,
                body,
            });
        }
    }

    async fn check_url(&self, url: &str) -> Result<Vec<SocketAddr>, RegistryError> {
        validate_install_url(url, self.policy.allow_loopback_http)
            .map_err(RegistryError::UnsafeUrl)?;
        let parsed = ::url::Url::parse(url).map_err(|error| {
            RegistryError::UnsafeUrl(InstallError::InvalidUrl {
                input: url.to_owned(),
                message: error.to_string(),
            })
        })?;
        let loopback = self.policy.allow_loopback_http && is_loopback_http_url(url);
        let port = parsed.port_or_known_default().unwrap_or(443);
        let host = match parsed.host() {
            Some(::url::Host::Ipv4(ip)) => {
                return check_addresses(
                    &ip.to_string(),
                    vec![SocketAddr::new(IpAddr::V4(ip), port)],
                    loopback,
                );
            }
            Some(::url::Host::Ipv6(ip)) => {
                return check_addresses(
                    &ip.to_string(),
                    vec![SocketAddr::new(IpAddr::V6(ip), port)],
                    loopback,
                );
            }
            Some(::url::Host::Domain(host)) => host.to_owned(),
            None => {
                return Err(RegistryError::UnsafeUrl(InstallError::MissingHost(
                    url.to_owned(),
                )));
            }
        };
        let addresses = self.resolver.resolve(&host, port).await.map_err(|error| {
            RegistryError::Transport(TransportError::Connect(format!(
                "dns lookup failed for {host:?}: {error}"
            )))
        })?;
        check_addresses(&host, addresses, loopback)
    }
}

fn check_addresses(
    host: &str,
    addresses: Vec<SocketAddr>,
    loopback: bool,
) -> Result<Vec<SocketAddr>, RegistryError> {
    if addresses.is_empty() {
        return Err(RegistryError::UnsafeUrl(InstallError::NoAddresses {
            host: host.to_owned(),
        }));
    }
    for address in &addresses {
        let blocked = if loopback {
            !address.ip().is_loopback()
        } else {
            match address.ip() {
                IpAddr::V4(ip) => is_non_global_v4(ip),
                IpAddr::V6(ip) => is_non_global_v6(ip),
            }
        };
        if blocked {
            return Err(RegistryError::UnsafeUrl(InstallError::NonPublicAddress {
                host: host.to_owned(),
                address: address.ip(),
            }));
        }
    }
    Ok(addresses)
}

fn is_credential_header(name: &str) -> bool {
    ["authorization", "proxy-authorization", "cookie"]
        .iter()
        .any(|credential| name.eq_ignore_ascii_case(credential))
}

fn same_origin(a: &str, b: &str) -> bool {
    match (::url::Url::parse(a), ::url::Url::parse(b)) {
        (Ok(a), Ok(b)) => a.origin() == b.origin(),
        _ => false,
    }
}

fn next_hop(current: &str, location: &str) -> Result<String, RegistryError> {
    let base = ::url::Url::parse(current).map_err(|error| RegistryError::Malformed {
        what: "redirect",
        detail: error.to_string(),
    })?;
    base.join(location.trim())
        .map(String::from)
        .map_err(|error| RegistryError::Malformed {
            what: "redirect",
            detail: error.to_string(),
        })
}

/// A fetched, validated and scanned `SKILL.md`.
///
/// The registry never stores it and never refuses it for its scan verdict:
/// [`is_blocked`](Self::is_blocked) tells the host, whose policy decides.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RegistryDocument {
    /// The catalog entry it was fetched for; `None` for an ad-hoc URL.
    pub entry: Option<SkillSummary>,
    /// The validated document, with its YAML frontmatter and install slug.
    pub document: FetchedDocument,
    /// The document read by the flat parser.
    pub flat: FlatSkill,
    /// [`document_digest`] of the document text.
    pub digest: String,
    /// The supply-chain scan of the document.
    pub scan: ScanReport,
    /// The URL it was fetched from, redacted.
    pub fetched_from: String,
}

impl RegistryDocument {
    /// Whether the scan verdict is block.
    #[must_use]
    pub fn is_blocked(&self) -> bool {
        self.scan.is_blocked()
    }
}

pub(crate) fn build_document(
    entry: Option<SkillSummary>,
    url: &str,
    body: &[u8],
) -> Result<RegistryDocument, RegistryError> {
    let document = validate_fetched_document(body).map_err(RegistryError::InvalidDocument)?;
    let flat = parse_flat(&document.content).map_err(|error| {
        RegistryError::InvalidDocument(match error {
            FlatError::MissingKeys { keys } => {
                DocumentError::MissingField(keys.first().copied().unwrap_or("name"))
            }
            _ => DocumentError::UnterminatedFrontmatter,
        })
    })?;
    let scan = scan_skill(&flat.scan_document(), &[]);
    Ok(RegistryDocument {
        entry,
        digest: document_digest(&document.content),
        document,
        flat,
        scan,
        fetched_from: redact_url(url),
    })
}

/// Fetch and validate one `SKILL.md` from a URL a user supplied.
///
/// The URL is normalized with [`normalize_registry_document_url`] (GitHub
/// blob links become raw links; `ClawHub`'s file API is accepted), then
/// fetched through the same guard a registry uses, within
/// [`RegistryTimeouts::document`] and [`RegistryLimits::max_document_bytes`].
///
/// # Errors
///
/// [`RegistryError::UnsafeUrl`] for a URL the guard refuses,
/// [`RegistryError::Unavailable`] or [`RegistryError::RateLimited`] for a
/// non-`2xx` answer, [`RegistryError::TooLarge`], [`RegistryError::Timeout`],
/// [`RegistryError::Transport`], and [`RegistryError::InvalidDocument`] when
/// the body is not a valid `SKILL.md`.
pub async fn fetch_skill_document(
    transport: Arc<dyn RegistryTransport>,
    resolver: Arc<dyn Resolver>,
    url: &str,
    policy: &FetchPolicy,
    timeouts: &RegistryTimeouts,
    limits: &RegistryLimits,
) -> Result<RegistryDocument, RegistryError> {
    let fetcher = GuardedFetcher::new(transport, resolver, policy.clone(), timeouts, limits);
    let url = url.trim();
    validate_install_url(url, policy.allow_loopback_http).map_err(RegistryError::UnsafeUrl)?;
    let url = normalize_registry_document_url(url).map_err(RegistryError::UnsafeUrl)?;
    let response = fetcher
        .fetch(FetchSpec {
            method: HttpMethod::Get,
            url: &url,
            headers: &[],
            max_bytes: limits.max_document_bytes,
            what: "document",
            budget: timeouts.document,
        })
        .await?
        .error_for_status()?;
    build_document(None, &response.url, &response.body)
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;
