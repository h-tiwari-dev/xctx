//! The DevTools HTTP endpoints (`/json/version`, `/json/list`, `/json/close/<id>`), over a plain blocking
//! socket: Chrome serves them on the debugging port, on the loopback address only.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use crate::error::{BrowserError, Result};

/// The most a DevTools HTTP answer may be; target lists are a few kilobytes.
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// `GET http://127.0.0.1:<port><path>` and the body, failing on anything but a 200.
pub(crate) fn get(port: u16, path: &str, timeout: Duration) -> Result<String> {
    let fail = |reason: String| BrowserError::Http {
        path: path.to_owned(),
        reason,
    };
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, timeout)
        .map_err(|e| fail(format!("could not connect to port {port}: {e}")))?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|e| fail(e.to_string()))?;
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|e| fail(e.to_string()))?;
    // Chrome keeps the connection open after answering, so read until the answer is complete, not to EOF.
    let mut raw = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    while !is_complete(&raw) {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(fail(e.to_string())),
        }
        if raw.len() > MAX_RESPONSE_BYTES {
            return Err(fail(format!(
                "the answer is over {MAX_RESPONSE_BYTES} bytes"
            )));
        }
    }
    parse_response(&raw).map_err(fail)
}

/// Whether `raw` holds a whole response: its headers and as much body as they announce. A response
/// announcing no length is complete only at EOF.
fn is_complete(raw: &[u8]) -> bool {
    let Some(split) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
        return false;
    };
    let head = String::from_utf8_lossy(&raw[..split]).to_ascii_lowercase();
    let body = &raw[split + 4..];
    for line in head.split("\r\n").skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        match (name.trim(), value.trim()) {
            ("content-length", n) => return n.parse::<usize>().is_ok_and(|n| body.len() >= n),
            ("transfer-encoding", "chunked") => return body.ends_with(b"0\r\n\r\n"),
            _ => {}
        }
    }
    false
}

/// The body of a raw HTTP/1.1 response if its status is 200, or why not.
pub(crate) fn parse_response(raw: &[u8]) -> std::result::Result<String, String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("the response has no end of headers")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let body = &raw[split + 4..];
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("bad status line {status_line:?}"))?;
    let mut length = None;
    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            length = value.parse::<usize>().ok();
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value.eq_ignore_ascii_case("chunked");
        }
    }
    let body = if chunked {
        dechunk(body)?
    } else {
        match length {
            Some(n) if n <= body.len() => body[..n].to_vec(),
            Some(n) => return Err(format!("expected {n} body bytes, got {}", body.len())),
            None => body.to_vec(),
        }
    };
    let body = String::from_utf8_lossy(&body).into_owned();
    if status != 200 {
        return Err(format!("status {status}: {}", body.trim()));
    }
    Ok(body)
}

fn dechunk(mut raw: &[u8]) -> std::result::Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let end = raw
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("a chunk has no size line")?;
        let size_text = String::from_utf8_lossy(&raw[..end]);
        let size_text = size_text.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| format!("bad chunk size {size_text:?}"))?;
        raw = &raw[end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if raw.len() < size {
            return Err("a chunk is cut short".to_owned());
        }
        out.extend_from_slice(&raw[..size]);
        raw = raw.get(size + 2..).unwrap_or_default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_200_with_a_length_gives_its_body() {
        let raw =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n[]";
        assert_eq!(parse_response(raw).as_deref(), Ok("[]"));
    }

    #[test]
    fn completeness_follows_the_announced_length() {
        assert!(!is_complete(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n"));
        assert!(!is_complete(
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n["
        ));
        assert!(is_complete(
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n[]"
        ));
        assert!(is_complete(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx\r\n0\r\n\r\n"
        ));
        assert!(!is_complete(b"HTTP/1.1 200 OK\r\n\r\nuntil eof"));
    }

    #[test]
    fn chunked_bodies_are_joined() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n[{}\r\n1;x=y\r\n]\r\n0\r\n\r\n";
        assert_eq!(parse_response(raw).as_deref(), Ok("[{}]"));
    }

    #[test]
    fn other_statuses_and_garbage_fail() {
        let err = parse_response(b"HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\n\r\nnope");
        assert!(err.is_err_and(|e| e.contains("404") && e.contains("nope")));
        assert!(parse_response(b"HTTP/1.1 200 OK\r\n").is_err());
        assert!(parse_response(b"garbage\r\n\r\n").is_err());
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nab").is_err());
    }
}
