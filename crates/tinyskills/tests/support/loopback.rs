//! A scripted HTTP/1.1 server on a loopback socket, and a minimal socket
//! transport that keeps the registry transport contract.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tinyskills::{
    BodyChunks, BoxFuture, HttpMethod, RegistryTransport, TransportError, TransportRequest,
    TransportResponse,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const MAX_HEAD_BYTES: usize = 64 * 1024;
const MAX_CHUNK_HEADER_BYTES: usize = 1024;

#[derive(Clone)]
pub(crate) enum Script {
    Plain {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    Chunked(Vec<Vec<u8>>),
    Drip {
        body: Vec<u8>,
        every: Duration,
    },
    Redirect(String),
}

pub(crate) struct Server {
    pub(crate) addr: SocketAddr,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Server {
    pub(crate) fn start(routes: Vec<(&str, Script)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let routes: HashMap<String, Script> = routes
            .into_iter()
            .map(|(path, script)| (path.to_owned(), script))
            .collect();
        let routes = Arc::new(routes);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let routes = Arc::clone(&routes);
                let log = Arc::clone(&log);
                std::thread::spawn(move || serve(stream, &routes, &log));
            }
        });
        Self { addr, seen }
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.addr.port())
    }

    pub(crate) fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

fn serve(stream: TcpStream, routes: &HashMap<String, Script>, log: &Mutex<Vec<String>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    log.lock().unwrap().push(format!("{method} {path}"));
    let mut out = stream;
    let head_only = method == "HEAD";
    let script = routes.get(&path).cloned().unwrap_or(Script::Plain {
        status: 404,
        headers: Vec::new(),
        body: b"missing".to_vec(),
    });
    let _ = match script {
        Script::Plain {
            status,
            headers,
            body,
        } => {
            let mut head = format!("HTTP/1.1 {status} X\r\nConnection: close\r\n");
            if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-length")) {
                let _ = write!(head, "Content-Length: {}\r\n", body.len());
            }
            for (name, value) in headers {
                let _ = write!(head, "{name}: {value}\r\n");
            }
            head.push_str("\r\n");
            out.write_all(head.as_bytes())
                .and_then(|()| if head_only { Ok(()) } else { out.write_all(&body) })
        }
        Script::Redirect(location) => out.write_all(
            format!(
                "HTTP/1.1 301 Moved\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        ),
        Script::Chunked(parts) => {
            let mut result = out.write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            );
            for part in parts.into_iter().filter(|_| !head_only) {
                result = result
                    .and_then(|()| out.write_all(format!("{:x}\r\n", part.len()).as_bytes()))
                    .and_then(|()| out.write_all(&part))
                    .and_then(|()| out.write_all(b"\r\n"));
            }
            result.and_then(|()| {
                if head_only {
                    Ok(())
                } else {
                    out.write_all(b"0\r\n\r\n")
                }
            })
        }
        Script::Drip { body, every } => {
            let mut result = out.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            );
            if !head_only {
                for byte in body {
                    std::thread::sleep(every);
                    result = result.and_then(|()| out.write_all(&[byte]));
                    if result.is_err() {
                        break;
                    }
                }
            }
            result
        }
    };
    let _ = out.flush();
}

#[derive(Default)]
pub(crate) struct SocketTransport {
    pub(crate) pinned_seen: Mutex<Vec<Vec<SocketAddr>>>,
}

enum Framing {
    Empty,
    Length(usize),
    Chunked,
}

struct SocketBody {
    stream: tokio::net::TcpStream,
    buffer: Vec<u8>,
    framing: Framing,
    done: bool,
}

impl SocketBody {
    async fn fill(&mut self) -> Result<bool, TransportError> {
        self.fill_capped(8192).await
    }

    async fn fill_capped(&mut self, cap: usize) -> Result<bool, TransportError> {
        let mut chunk = [0_u8; 8192];
        let read = self
            .stream
            .read(&mut chunk[..cap.clamp(1, 8192)])
            .await
            .map_err(|e| TransportError::Io(e.to_string()))?;
        self.buffer.extend_from_slice(&chunk[..read]);
        Ok(read > 0)
    }

    async fn line(&mut self) -> Result<String, TransportError> {
        loop {
            if let Some(end) = self.buffer.windows(2).position(|w| w == b"\r\n") {
                if end > MAX_CHUNK_HEADER_BYTES {
                    return Err(TransportError::Io("chunk header too large".to_owned()));
                }
                let line = std::str::from_utf8(&self.buffer[..end])
                    .map_err(|e| TransportError::Io(e.to_string()))?
                    .to_owned();
                self.buffer.drain(..end + 2);
                return Ok(line);
            }
            let remaining = (MAX_CHUNK_HEADER_BYTES + 2).saturating_sub(self.buffer.len());
            if remaining == 0 {
                return Err(TransportError::Io("chunk header too large".to_owned()));
            }
            if !self.fill_capped(remaining).await? {
                return Err(TransportError::Io("eof in chunk header".to_owned()));
            }
        }
    }

    async fn exact(&mut self, len: usize) -> Result<Vec<u8>, TransportError> {
        while self.buffer.len() < len {
            if !self.fill().await? {
                return Err(TransportError::Io("eof in body".to_owned()));
            }
        }
        Ok(self.buffer.drain(..len).collect())
    }

    async fn next(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        if self.done {
            return Ok(None);
        }
        match self.framing {
            Framing::Empty | Framing::Length(0) => {
                self.done = true;
                Ok(None)
            }
            Framing::Length(remaining) => {
                if self.buffer.is_empty() && !self.fill().await? {
                    return Err(TransportError::Io("eof in body".to_owned()));
                }
                let take = remaining.min(self.buffer.len());
                self.framing = Framing::Length(remaining - take);
                Ok(Some(self.buffer.drain(..take).collect()))
            }
            Framing::Chunked => {
                let size = usize::from_str_radix(self.line().await?.trim(), 16)
                    .map_err(|e| TransportError::Io(e.to_string()))?;
                if size == 0 {
                    self.done = true;
                    return Ok(None);
                }
                if size > 256 * 1024 * 1024 {
                    return Err(TransportError::Io("chunk too large".to_owned()));
                }
                let data = self.exact(size).await?;
                if self.exact(2).await? != b"\r\n" {
                    return Err(TransportError::Io("bad chunk terminator".to_owned()));
                }
                Ok(Some(data))
            }
        }
    }
}

impl BodyChunks for SocketBody {
    fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, TransportError>> {
        Box::pin(self.next())
    }
}

impl RegistryTransport for SocketTransport {
    fn send(
        &self,
        request: TransportRequest,
    ) -> BoxFuture<'_, Result<TransportResponse, TransportError>> {
        self.pinned_seen
            .lock()
            .unwrap()
            .push(request.pinned.clone());
        Box::pin(async move {
            let url =
                url::Url::parse(&request.url).map_err(|e| TransportError::Io(e.to_string()))?;
            let connect = tokio::net::TcpStream::connect(request.pinned[0]);
            let mut stream = tokio::time::timeout(request.connect_timeout, connect)
                .await
                .map_err(|_| TransportError::Timeout)?
                .map_err(|e| TransportError::Connect(e.to_string()))?;
            let mut head = format!(
                "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
                request.method.as_str(),
                &url[url::Position::BeforePath..],
                url.host_str().unwrap_or_default()
            );
            for (name, value) in &request.headers {
                let _ = write!(head, "{name}: {value}\r\n");
            }
            head.push_str("\r\n");
            stream
                .write_all(head.as_bytes())
                .await
                .map_err(|e| TransportError::Io(e.to_string()))?;
            let mut body = SocketBody {
                stream,
                buffer: Vec::new(),
                framing: Framing::Empty,
                done: false,
            };
            let end = loop {
                if let Some(pos) = body.buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                    if pos > MAX_HEAD_BYTES {
                        return Err(TransportError::Io("head too large".to_owned()));
                    }
                    break pos;
                }
                let remaining = (MAX_HEAD_BYTES + 4).saturating_sub(body.buffer.len());
                if remaining == 0 {
                    return Err(TransportError::Io("head too large".to_owned()));
                }
                if !body.fill_capped(remaining).await? {
                    return Err(TransportError::Io("eof in head".to_owned()));
                }
            };
            let head = std::str::from_utf8(&body.buffer[..end])
                .map_err(|e| TransportError::Io(e.to_string()))?
                .to_owned();
            body.buffer.drain(..end + 4);
            let mut lines = head.split("\r\n");
            let status = lines
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|code| code.parse().ok())
                .ok_or_else(|| TransportError::Io("bad status line".to_owned()))?;
            let headers: Vec<(String, String)> = lines
                .filter_map(|line| line.split_once(':'))
                .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
                .collect();
            let header = |name: &str| {
                headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(name))
                    .map(|(_, v)| v.clone())
            };
            body.framing = if request.method == HttpMethod::Head {
                Framing::Empty
            } else if header("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked"))
            {
                Framing::Chunked
            } else {
                Framing::Length(
                    header("content-length")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0),
                )
            };
            Ok(TransportResponse::new(
                status,
                request.url,
                headers,
                Box::new(body),
            ))
        })
    }
}
