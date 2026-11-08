//! Just enough HTTP/1.1 for a fixture to call its sidecar's control API ([`crate::Sidecar`]):
//! one request per connection, a JSON body, and a response framed by `Content-Length`, chunked
//! encoding or the connection closing.

use std::io::{ErrorKind, Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};

use crate::process;

/// How often a blocked read wakes up to check the deadline and ctrl-c.
const POLL: Duration = Duration::from_millis(100);

/// Refuse to buffer more than this: a mock's request log (fc's, with images) stays far below.
const MAX_RESPONSE: usize = 256 * 1024 * 1024;

/// A sidecar's reply: its status and its whole body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// A 2xx status.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Send `method path` with an optional JSON `body` to `addr` and read the
/// whole response before `deadline`. Fails with "interrupted" on ctrl-c.
pub(crate) fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    deadline: Instant,
) -> Result<HttpResponse> {
    let left = remaining(deadline).context("timed out before connecting")?;
    let mut stream =
        TcpStream::connect_timeout(&addr, left).with_context(|| format!("connecting to {addr}"))?;
    stream.set_nodelay(true).ok();
    stream
        .set_write_timeout(Some(
            remaining(deadline).context("timed out sending the request")?,
        ))
        .context("setting the write timeout")?;
    stream
        .write_all(&request_bytes(method, addr, path, body))
        .and_then(|()| stream.flush())
        .context("sending the request")?;

    let mut buf = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        if process::interrupted() {
            bail!("interrupted");
        }
        let Some(left) = remaining(deadline) else {
            bail!(
                "timed out waiting for the response ({} bytes so far)",
                buf.len()
            );
        };
        stream
            .set_read_timeout(Some(left.min(POLL)))
            .context("setting the read timeout")?;
        match stream.read(&mut chunk) {
            Ok(0) => {
                return parse_response(&buf, true)?
                    .ok_or_else(|| anyhow!("the connection closed mid-response"));
            }
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > MAX_RESPONSE {
                    bail!("the response exceeds {MAX_RESPONSE} bytes");
                }
                if let Some(response) = parse_response(&buf, false)? {
                    return Ok(response);
                }
            }
            Err(err)
                if matches!(
                    err.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) => {}
            Err(err) => return Err(err).context("reading the response"),
        }
    }
}

/// Time left before `deadline`, `None` once it has passed.
fn remaining(deadline: Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
}

/// The request as sent: `Connection: close` so the server ends the exchange,
/// and a length on every POST (servers may reject a bodiless POST without it).
fn request_bytes(method: &str, addr: SocketAddr, path: &str, body: Option<&[u8]>) -> Vec<u8> {
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nAccept: application/json\r\n"
    );
    if let Some(body) = body {
        head.push_str("Content-Type: application/json\r\n");
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    } else if method.eq_ignore_ascii_case("POST") {
        head.push_str("Content-Length: 0\r\n");
    }
    head.push_str("\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body.unwrap_or_default());
    bytes
}

/// Parse a response from the bytes received so far. `Ok(None)` means more
/// bytes are needed; `eof` says none will come.
fn parse_response(buf: &[u8], eof: bool) -> Result<Option<HttpResponse>> {
    let Some(head_len) = find(buf, b"\r\n\r\n") else {
        if eof {
            bail!("the connection closed before the response headers ended");
        }
        return Ok(None);
    };
    let head = std::str::from_utf8(&buf[..head_len]).context("response headers are not UTF-8")?;
    let rest = &buf[head_len + 4..];
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = parse_status_line(status_line)?;
    if (100..200).contains(&status) {
        // An interim response (e.g. 100 Continue); the real one follows.
        return parse_response(rest, eof);
    }

    let mut content_length = None;
    let mut chunked = false;
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| anyhow!("malformed response header {line:?}"))?;
        let (name, value) = (name.trim(), value.trim());
        if name.eq_ignore_ascii_case("content-length") {
            content_length = Some(
                value
                    .parse::<usize>()
                    .with_context(|| format!("bad Content-Length {value:?}"))?,
            );
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value
                .split(',')
                .any(|coding| coding.trim().eq_ignore_ascii_case("chunked"));
        }
    }

    let body = if status == 204 || status == 304 {
        Some(Vec::new())
    } else if chunked {
        decode_chunked(rest)?
    } else if let Some(length) = content_length {
        if rest.len() >= length {
            Some(rest[..length].to_vec())
        } else if eof {
            bail!(
                "the connection closed after {} of {length} body bytes",
                rest.len()
            );
        } else {
            None
        }
    } else if eof {
        Some(rest.to_vec())
    } else {
        None
    };
    match body {
        Some(body) => Ok(Some(HttpResponse { status, body })),
        None if eof => bail!("the connection closed mid-body"),
        None => Ok(None),
    }
}

fn parse_status_line(line: &str) -> Result<u16> {
    let mut parts = line.split_whitespace();
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") {
        bail!("not an HTTP/1.x response: {line:?}");
    }
    parts
        .next()
        .filter(|code| code.len() == 3)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| anyhow!("bad status line {line:?}"))
}

/// Decode a chunked body. `Ok(None)` while the final chunk hasn't arrived.
fn decode_chunked(data: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut out = Vec::new();
    let mut pos = 0;
    loop {
        let Some(line_len) = find(&data[pos..], b"\r\n") else {
            return Ok(None);
        };
        let line = std::str::from_utf8(&data[pos..pos + line_len])
            .context("chunk size line is not UTF-8")?;
        /* Chunk extensions (`;name=value`) carry nothing we need. */
        let size_text = line.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16)
            .with_context(|| format!("bad chunk size {size_text:?}"))?;
        pos += line_len + 2;
        if size == 0 {
            /* Optional trailers, then the empty line that ends the body. */
            loop {
                let Some(trailer_len) = find(&data[pos..], b"\r\n") else {
                    return Ok(None);
                };
                if trailer_len == 0 {
                    return Ok(Some(out));
                }
                pos += trailer_len + 2;
            }
        }
        let end = pos
            .checked_add(size)
            .filter(|end| *end <= MAX_RESPONSE)
            .ok_or_else(|| anyhow!("chunk size {size} is too large"))?;
        if data.len() < end + 2 {
            return Ok(None);
        }
        if &data[end..end + 2] != b"\r\n" {
            bail!("a chunk is not followed by CRLF");
        }
        out.extend_from_slice(&data[pos..end]);
        pos = end + 2;
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::io::{BufRead as _, BufReader};
    use std::net::TcpListener;

    fn complete(bytes: &[u8]) -> HttpResponse {
        parse_response(bytes, false)
            .unwrap()
            .expect("a complete response")
    }

    #[test]
    fn content_length_bodies_wait_for_every_byte() {
        let full = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\ncontent-length: 7\r\n\r\n{\"a\":1}";
        for cut in 0..full.len() {
            assert_eq!(parse_response(&full[..cut], false).unwrap(), None, "{cut}");
        }
        assert_eq!(
            complete(full),
            HttpResponse {
                status: 200,
                body: b"{\"a\":1}".to_vec(),
            }
        );
        let err = parse_response(&full[..full.len() - 2], true).unwrap_err();
        assert!(err.to_string().contains("after 5 of 7"), "{err}");
    }

    #[test]
    fn chunked_bodies_are_decoded() {
        let full = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4;ext=1\r\n[1,2\r\nA\r\n,3,4,5,6,7\r\n1\r\n]\r\n0\r\nX-Trailer: y\r\n\r\n";
        for cut in 0..full.len() {
            assert_eq!(parse_response(&full[..cut], false).unwrap(), None, "{cut}");
        }
        let response = complete(full);
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"[1,2,3,4,5,6,7]");

        let bad = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n";
        assert!(parse_response(bad, false).is_err());
        let unterminated = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nabXY";
        assert!(parse_response(unterminated, false).is_err());
    }

    #[test]
    fn unframed_bodies_end_with_the_connection() {
        let full = b"HTTP/1.0 404 Not Found\r\n\r\nnope";
        assert_eq!(parse_response(full, false).unwrap(), None);
        assert_eq!(
            parse_response(full, true).unwrap(),
            Some(HttpResponse {
                status: 404,
                body: b"nope".to_vec(),
            })
        );
        let no_content = complete(b"HTTP/1.1 204 No Content\r\n\r\n");
        assert_eq!(no_content.body, b"");
    }

    #[test]
    fn interim_responses_are_skipped() {
        let full =
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 201 Created\r\nContent-Length: 2\r\n\r\n{}";
        assert_eq!(complete(full).status, 201);
    }

    #[test]
    fn malformed_responses_are_errors() {
        for bytes in [
            &b"SSH-2.0-OpenSSH\r\n\r\n"[..],
            b"HTTP/1.1 twohundred OK\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nno colon here\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: many\r\n\r\n",
        ] {
            assert!(
                parse_response(bytes, true).is_err(),
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
        assert!(parse_response(b"HTTP/1.1 200", true).is_err());
    }

    #[test]
    fn requests_carry_host_length_and_close() {
        let addr: SocketAddr = "127.0.0.1:4010".parse().unwrap();
        assert_eq!(
            String::from_utf8(request_bytes(
                "POST",
                addr,
                "/__mock/catalog",
                Some(br#"{"set":"v2"}"#)
            ))
            .unwrap(),
            "POST /__mock/catalog HTTP/1.1\r\nHost: 127.0.0.1:4010\r\nConnection: close\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: 12\r\n\r\n{\"set\":\"v2\"}"
        );
        assert_eq!(
            String::from_utf8(request_bytes("POST", addr, "/__mock/reset", None)).unwrap(),
            "POST /__mock/reset HTTP/1.1\r\nHost: 127.0.0.1:4010\r\nConnection: close\r\nAccept: application/json\r\nContent-Length: 0\r\n\r\n"
        );
        assert_eq!(
            String::from_utf8(request_bytes("GET", addr, "/__mock/state", None)).unwrap(),
            "GET /__mock/state HTTP/1.1\r\nHost: 127.0.0.1:4010\r\nConnection: close\r\nAccept: application/json\r\n\r\n"
        );
    }

    /// Accept one connection, hand back the request head and body, and reply
    /// with `response` written in small pieces.
    fn serve_once(response: &'static [u8]) -> (SocketAddr, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut request = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
                request.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            request.push_str(&String::from_utf8(body).unwrap());
            let mut stream = reader.into_inner();
            for piece in response.chunks(5) {
                stream.write_all(piece).unwrap();
                stream.flush().unwrap();
                std::thread::sleep(Duration::from_millis(2));
            }
            request
        });
        (addr, server)
    }

    #[test]
    fn round_trips_over_tcp() {
        let (addr, server) = serve_once(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nC\r\n{\"ok\": true}\r\n0\r\n\r\n",
        );
        let response = request(
            addr,
            "POST",
            "/__mock/speed",
            Some(br#"{"speed":0}"#),
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, br#"{"ok": true}"#);
        let sent = server.join().unwrap();
        assert!(
            sent.starts_with("POST /__mock/speed HTTP/1.1\r\n"),
            "{sent}"
        );
        assert!(sent.ends_with("\r\n\r\n{\"speed\":0}"), "{sent}");
    }

    #[test]
    fn a_silent_server_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let started = Instant::now();
        let err = request(
            addr,
            "GET",
            "/__mock/state",
            None,
            started + Duration::from_millis(300),
        )
        .unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err:#}");
        assert!(started.elapsed() < Duration::from_secs(5));
        drop(listener);
    }

    #[test]
    fn a_closed_port_is_an_error() {
        let addr = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap()
        };
        let err = request(
            addr,
            "GET",
            "/__mock/state",
            None,
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("connecting to"), "{err:#}");
    }
}
