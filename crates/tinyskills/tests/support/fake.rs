//! In-process test doubles for the registry's host boundary.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use tinyskills::{
    BodyChunks, BoxFuture, Clock, HttpMethod, RegistryTransport, Resolver, TransportError,
    TransportRequest, TransportResponse,
};

pub(crate) const PUBLIC_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34));

#[derive(Clone, Debug)]
pub(crate) enum Reply {
    Respond {
        status: u16,
        headers: Vec<(String, String)>,
        chunks: Vec<Vec<u8>>,
        delay: Duration,
        final_url: Option<String>,
    },
    Fail(TransportError),
    Hang,
}

impl Reply {
    pub(crate) fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self::status(200, body)
    }

    pub(crate) fn status(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self::Respond {
            status,
            headers: Vec::new(),
            chunks: vec![body.into()],
            delay: Duration::ZERO,
            final_url: None,
        }
    }

    pub(crate) fn redirect(status: u16, location: &str) -> Self {
        Self::status(status, Vec::new()).header("Location", location)
    }

    pub(crate) fn header(mut self, name: &str, value: &str) -> Self {
        if let Self::Respond { headers, .. } = &mut self {
            headers.push((name.to_owned(), value.to_owned()));
        }
        self
    }

    pub(crate) fn chunked(mut self, parts: Vec<Vec<u8>>) -> Self {
        if let Self::Respond { chunks, .. } = &mut self {
            *chunks = parts;
        }
        self
    }

    pub(crate) fn delayed(mut self, by: Duration) -> Self {
        if let Self::Respond { delay, .. } = &mut self {
            *delay = by;
        }
        self
    }

    pub(crate) fn answering_for(mut self, url: &str) -> Self {
        if let Self::Respond { final_url, .. } = &mut self {
            *final_url = Some(url.to_owned());
        }
        self
    }
}

#[derive(Default)]
pub(crate) struct FakeTransport {
    routes: Mutex<HashMap<(String, String), VecDeque<Reply>>>,
    requests: Mutex<Vec<TransportRequest>>,
}

fn method_key(method: HttpMethod) -> String {
    method.as_str().to_owned()
}

impl FakeTransport {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn route(&self, method: HttpMethod, url: &str, replies: Vec<Reply>) {
        self.routes
            .lock()
            .unwrap()
            .insert((method_key(method), url.to_owned()), replies.into());
    }

    pub(crate) fn get(&self, url: &str, reply: Reply) {
        self.route(HttpMethod::Get, url, vec![reply]);
    }

    pub(crate) fn requests(&self) -> Vec<TransportRequest> {
        self.requests.lock().unwrap().clone()
    }

    pub(crate) fn count(&self, url: &str) -> usize {
        self.requests().iter().filter(|r| r.url == url).count()
    }
}

struct ChunkBody(VecDeque<Vec<u8>>);

impl BodyChunks for ChunkBody {
    fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, TransportError>> {
        let next = self.0.pop_front();
        Box::pin(async move { Ok(next) })
    }
}

impl RegistryTransport for FakeTransport {
    fn send(
        &self,
        request: TransportRequest,
    ) -> BoxFuture<'_, Result<TransportResponse, TransportError>> {
        assert!(
            !request.pinned.is_empty(),
            "request sent without pinned addresses"
        );
        self.requests.lock().unwrap().push(request.clone());
        let reply = {
            let mut routes = self.routes.lock().unwrap();
            let key = (method_key(request.method), request.url.clone());
            match routes.get_mut(&key) {
                Some(queue) if queue.len() > 1 => queue.pop_front(),
                Some(queue) => queue.front().cloned(),
                None => None,
            }
        };
        Box::pin(async move {
            match reply {
                None => Ok(TransportResponse::new(
                    404,
                    request.url,
                    Vec::new(),
                    Box::new(ChunkBody(VecDeque::new())),
                )),
                Some(Reply::Fail(error)) => Err(error),
                Some(Reply::Hang) => std::future::pending().await,
                Some(Reply::Respond {
                    status,
                    headers,
                    chunks,
                    delay,
                    final_url,
                }) => {
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                    Ok(TransportResponse::new(
                        status,
                        final_url.unwrap_or(request.url),
                        headers,
                        Box::new(ChunkBody(chunks.into())),
                    ))
                }
            }
        })
    }
}

#[derive(Default)]
pub(crate) struct FakeResolver {
    answers: Mutex<HashMap<String, Vec<IpAddr>>>,
    lookups: Mutex<Vec<String>>,
}

impl FakeResolver {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn answer(&self, host: &str, ips: Vec<IpAddr>) {
        self.answers.lock().unwrap().insert(host.to_owned(), ips);
    }

    pub(crate) fn lookups(&self) -> Vec<String> {
        self.lookups.lock().unwrap().clone()
    }
}

impl Resolver for FakeResolver {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> BoxFuture<'a, std::io::Result<Vec<SocketAddr>>> {
        self.lookups.lock().unwrap().push(host.to_owned());
        let answer = self.answers.lock().unwrap().get(host).cloned();
        Box::pin(async move {
            match answer {
                Some(ips) if ips.is_empty() => Err(std::io::Error::other("nxdomain")),
                Some(ips) => Ok(ips
                    .into_iter()
                    .map(|ip| SocketAddr::new(ip, port))
                    .collect()),
                None => Ok(vec![SocketAddr::new(PUBLIC_IP, port)]),
            }
        })
    }
}

pub(crate) struct ManualClock(Mutex<SystemTime>);

impl ManualClock {
    pub(crate) fn new() -> Self {
        Self(Mutex::new(
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000),
        ))
    }

    pub(crate) fn advance(&self, by: Duration) {
        *self.0.lock().unwrap() += by;
    }

    pub(crate) fn unix(&self) -> u64 {
        self.now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
}

impl Clock for ManualClock {
    fn now(&self) -> SystemTime {
        *self.0.lock().unwrap()
    }
}
