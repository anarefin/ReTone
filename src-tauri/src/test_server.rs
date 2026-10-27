//! A tiny HTTP/1.1 server on 127.0.0.1 for exercising the real reqwest paths
//! in tests. One request per connection; every request is recorded.

use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

pub enum Reply {
    Full {
        status: u16,
        headers: Vec<(String, String)>,
        body: String,
    },
    /// Sends the status line and headers, then never the promised body.
    StallBody { status: u16 },
}

impl Reply {
    pub fn status(status: u16) -> Self {
        Self::Full {
            status,
            headers: Vec::new(),
            body: String::new(),
        }
    }

    pub fn json(status: u16, body: Value) -> Self {
        Self::Full {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: body.to_string(),
        }
    }

    pub fn text(status: u16, body: &str, headers: &[(&str, &str)]) -> Self {
        Self::Full {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: body.to_string(),
        }
    }

    pub fn redirect(status: u16, location: &str) -> Self {
        Self::text(status, "", &[("Location", location)])
    }
}

pub struct MockServer {
    pub url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl MockServer {
    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<Recorded> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut start = lines.next()?.split(' ');
    let method = start.next()?.to_string();
    let path = start.next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some(Recorded {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).to_string(),
    })
}

pub async fn serve<F>(handler: F) -> MockServer
where
    F: Fn(&Recorded) -> Reply + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let handler = Arc::new(handler);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let recorded = Arc::clone(&recorded);
            let handler = Arc::clone(&handler);
            tokio::spawn(async move {
                let Some(request) = read_request(&mut stream).await else {
                    return;
                };
                recorded.lock().unwrap().push(request.clone());
                match handler(&request) {
                    Reply::Full {
                        status,
                        headers,
                        body,
                    } => {
                        let mut out = format!(
                            "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                            body.len()
                        );
                        for (k, v) in headers {
                            out.push_str(&format!("{k}: {v}\r\n"));
                        }
                        out.push_str("\r\n");
                        out.push_str(&body);
                        let _ = stream.write_all(out.as_bytes()).await;
                        let _ = stream.shutdown().await;
                    }
                    Reply::StallBody { status } => {
                        let head = format!(
                            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{{"
                        );
                        let _ = stream.write_all(head.as_bytes()).await;
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    }
                }
            });
        }
    });
    MockServer { url, requests }
}

/// A local URL nothing listens on.
pub async fn closed_port_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    url
}
