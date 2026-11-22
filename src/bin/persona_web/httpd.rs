//! One-request-per-connection HTTP/1.1 helpers for the local UI.

use persona_core::Error;
use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;

const MAX_BODY: usize = 1 << 30; // 1 GiB: chat exports and datasets can be large

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub host: String,
    pub origin: Option<String>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn query(&self, k: &str) -> Option<&str> {
        self.query.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
    }
}

pub fn percent_decode(s: &str, plus_is_space: bool) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 3 <= b.len() => {
                let hex = |c: u8| (c as char).to_digit(16);
                match (hex(b[i + 1]), hex(b[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                        continue;
                    }
                    _ => out.push(b'%'),
                }
            }
            b'+' if plus_is_space => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

pub fn read_request(stream: &TcpStream) -> io::Result<Option<Request>> {
    let mut r = BufReader::new(stream);
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().ok_or_else(|| bad("no method"))?.to_string();
    let target = parts.next().ok_or_else(|| bad("no target"))?;
    let (path, qs) = target.split_once('?').unwrap_or((target, ""));
    let query = qs
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (percent_decode(k, true), percent_decode(v, true))
        })
        .collect();
    let path = path.to_string();

    let (mut host, mut origin, mut len) = (String::new(), None, 0usize);
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim();
            match k.trim().to_ascii_lowercase().as_str() {
                "host" => host = v.to_string(),
                "origin" => origin = Some(v.to_string()),
                "content-length" => len = v.parse().map_err(|_| bad("bad content-length"))?,
                _ => {}
            }
        }
    }
    if len > MAX_BODY {
        return Err(bad("body too large"));
    }
    let mut body = Vec::new();
    r.take(len as u64).read_to_end(&mut body)?;
    if body.len() < len {
        return Err(bad("request body ended early"));
    }
    Ok(Some(Request { method, path, query, host, origin, body }))
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        _ => "",
    }
}

pub fn send(mut s: &TcpStream, status: u16, ctype: &str, body: &[u8]) -> io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        reason(status),
        body.len()
    );
    s.write_all(head.as_bytes())?;
    s.write_all(body)
}

pub fn send_json(s: &TcpStream, status: u16, v: &Value) -> io::Result<()> {
    send(s, status, "application/json; charset=utf-8", v.to_string().as_bytes())
}

pub fn send_error(s: &TcpStream, e: &Error) -> io::Result<()> {
    let status = match e {
        Error::NotFound(_) => 404,
        Error::Exists(_) => 409,
        Error::Invalid(_) | Error::Json(_) => 400,
        Error::Http(..) | Error::Backend(_) => 502,
        Error::Io(_) => 500,
    };
    send_json(s, status, &json!({ "error": e.to_string() }))
}

/// Newline-delimited JSON, sent as it is produced.
pub struct Stream<'a>(&'a TcpStream);

impl<'a> Stream<'a> {
    pub fn start(mut s: &'a TcpStream) -> io::Result<Self> {
        s.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson; charset=utf-8\r\nCache-Control: no-store\r\nX-Accel-Buffering: no\r\nConnection: close\r\n\r\n",
        )?;
        Ok(Self(s))
    }

    pub fn line(&mut self, v: &Value) -> io::Result<()> {
        let mut b = v.to_string().into_bytes();
        b.push(b'\n');
        self.0.write_all(&b)?;
        self.0.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoding() {
        assert_eq!(percent_decode("a%20b%2Fc+d", false), "a b/c+d");
        assert_eq!(percent_decode("a+b", true), "a b");
        assert_eq!(percent_decode("%ED%95%B4%EA%B0%95", false), "해강");
        assert_eq!(percent_decode("100%", false), "100%");
        assert_eq!(percent_decode("%zz", false), "%zz");
    }
}
