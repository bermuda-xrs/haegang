//! A small HTTP/1.1 client for local model servers, on `std::net`. Bodies stream through `BufRead`, as
//! token streams need. Plain `http://` only: put a local proxy in front of anything remote.

use crate::{Error, Result};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

pub(crate) struct Response {
    pub status: u16,
    pub body: Box<dyn BufRead>,
}

impl Response {
    pub fn text(mut self) -> Result<String> {
        let mut s = String::new();
        self.body.read_to_string(&mut s).map_err(|e| Error::Io(format!("reading response: {e}")))?;
        Ok(s)
    }

    pub fn ok(self) -> Result<Self> {
        if (200..300).contains(&self.status) {
            return Ok(self);
        }
        let status = self.status;
        let body = self.text().unwrap_or_default();
        Err(Error::Http(status, body))
    }
}

fn split_url(base: &str) -> Result<(String, String)> {
    let rest = base
        .strip_prefix("http://")
        .ok_or_else(|| Error::Invalid(format!("`{base}`: only http:// is supported (use a local proxy for TLS)")))?;
    let (hp, prefix) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].trim_end_matches('/')),
        None => (rest, ""),
    };
    let hp = if hp.contains(':') { hp.to_string() } else { format!("{hp}:80") };
    Ok((hp, prefix.to_string()))
}

pub(crate) fn send(
    base: &str,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Option<&[u8]>,
    read_timeout: Duration,
) -> Result<Response> {
    let (hostport, prefix) = split_url(base)?;
    let io_err = |e: io::Error| Error::Io(format!("{hostport}: {e}"));

    // "localhost" can resolve to ::1 while the server listens on IPv4 only: try every address
    let mut stream = None;
    let mut last = None;
    for addr in hostport.to_socket_addrs().map_err(io_err)? {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(3)) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last = Some(e),
        }
    }
    let mut stream = stream.ok_or_else(|| io_err(last.unwrap_or_else(|| io::Error::other("no address"))))?;
    stream.set_read_timeout(Some(read_timeout)).map_err(io_err)?;
    stream.set_nodelay(true).ok();

    let host = hostport.rsplit_once(':').map_or(hostport.as_str(), |x| x.0);
    let mut head = format!("{method} {prefix}{path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nAccept: */*\r\n");
    if let Some(k) = bearer {
        head.push_str(&format!("Authorization: Bearer {k}\r\n"));
    }
    if let Some(b) = body {
        head.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", b.len()));
    }
    head.push_str("\r\n");
    let mut req = head.into_bytes();
    req.extend_from_slice(body.unwrap_or_default());
    stream.write_all(&req).map_err(io_err)?; // single write: one packet, no head/body split

    let mut r = BufReader::new(stream);
    let mut line = String::new();
    r.read_line(&mut line).map_err(io_err)?;
    let status: u16 = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| Error::Io(format!("bad HTTP status line: {line:?}")))?;

    let (mut chunked, mut len) = (false, None);
    loop {
        line.clear();
        if r.read_line(&mut line).map_err(io_err)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            } else if k == "content-length" {
                len = v.parse::<u64>().ok();
            }
        }
    }
    let body: Box<dyn BufRead> = if chunked {
        Box::new(BufReader::new(Chunked { inner: r, left: 0, done: false }))
    } else if let Some(n) = len {
        Box::new(BufReader::new(r.take(n)))
    } else {
        Box::new(r)
    };
    Ok(Response { status, body })
}

struct Chunked<R> {
    inner: R,
    left: usize,
    done: bool,
}

impl<R: BufRead> Read for Chunked<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            let mut line = String::new();
            self.inner.read_line(&mut line)?;
            let hex = line.trim().split(';').next().unwrap_or("");
            let n = usize::from_str_radix(hex, 16)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
            if n == 0 {
                self.done = true;
                return Ok(0);
            }
            self.left = n;
        }
        let want = buf.len().min(self.left);
        let got = self.inner.read(&mut buf[..want])?;
        if got == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.left -= got;
        if self.left == 0 {
            let mut crlf = [0u8; 2];
            self.inner.read_exact(&mut crlf)?;
        }
        Ok(got)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn serve(response: &'static [u8]) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            s.write_all(response).unwrap();
        });
        format!("http://{addr}/v1")
    }

    #[test]
    fn chunked_lines_split_across_chunks() {
        // line "{\"a\":1}\n" is split across chunk boundaries
        let base = serve(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n5\r\n:1}\n{\r\n5\r\n\"b\":2\r\n3\r\n}\n\n\r\n0\r\n\r\n");
        let r = send(&base, "POST", "/x", None, Some(b"{}"), Duration::from_secs(5)).unwrap().ok().unwrap();
        let lines: Vec<String> = r.body.lines().map(|l| l.unwrap()).collect();
        assert_eq!(lines, vec!["{\"a\":1}", "{\"b\":2}", ""]);
    }

    #[test]
    fn content_length_and_error_status() {
        let base = serve(b"HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\n\r\nnope");
        match send(&base, "GET", "/x", None, None, Duration::from_secs(5)).unwrap().ok() {
            Err(Error::Http(404, b)) => assert_eq!(b, "nope"),
            _ => panic!("expected 404"),
        }
        assert!(send("https://x", "GET", "/", None, None, Duration::from_secs(1)).is_err());
    }
}
