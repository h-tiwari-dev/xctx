//! The DevTools protocol over a websocket: framing requests, matching answers to them by id, and handing
//! every event to the caller as it arrives so nothing is queued without bound.

use std::io::ErrorKind;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Message, WebSocket};

use crate::error::{BrowserError, Result};

/// The most one message from Chrome may be. A full-page screenshot arrives as one base64 message, so this is
/// generous; anything bigger fails the call instead of growing memory further.
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024 * 1024;

/// How long connecting and the websocket handshake may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long past its deadline [`Connection::pump`] keeps reading events that are already waiting.
const DRAIN_GRACE: Duration = Duration::from_millis(250);

/// An event Chrome sent (a message without an `id`).
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// The CDP event name, for example `Page.loadEventFired`.
    pub method: String,
    /// Its parameters (`null` when there were none).
    pub params: Value,
}

/// A message from Chrome, decoded.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Incoming {
    /// The answer to the request with this id: its `result`, or its `error` as `(code, message)`.
    Response {
        id: u64,
        result: std::result::Result<Value, (i64, String)>,
    },
    /// An event.
    Event(Event),
}

/// The text of a CDP request.
pub(crate) fn encode_request(id: u64, method: &str, params: &Value) -> String {
    let params = if params.is_null() {
        json!({})
    } else {
        params.clone()
    };
    json!({ "id": id, "method": method, "params": params }).to_string()
}

/// Decodes one message from Chrome.
pub(crate) fn decode(text: &str) -> Result<Incoming> {
    let value: Value = serde_json::from_str(text)
        .map_err(|e| BrowserError::BadMessage(format!("not JSON ({e})")))?;
    if let Some(id) = value.get("id") {
        let id = id
            .as_u64()
            .ok_or_else(|| BrowserError::BadMessage(format!("bad id {id}")))?;
        let result = match value.get("error") {
            Some(error) => Err((
                error.get("code").and_then(Value::as_i64).unwrap_or(0),
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_owned(),
            )),
            None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
        };
        return Ok(Incoming::Response { id, result });
    }
    let method = value
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| BrowserError::BadMessage("neither an answer nor an event".to_owned()))?;
    Ok(Incoming::Event(Event {
        method: method.to_owned(),
        params: value.get("params").cloned().unwrap_or(Value::Null),
    }))
}

/// One websocket connection to a DevTools endpoint (the browser's or one page's).
pub(crate) struct Connection {
    ws: WebSocket<TcpStream>,
    next_id: u64,
}

impl Connection {
    /// Connects to `ws_url`, which must be a `ws://` URL on the loopback address (`127.0.0.1`, `[::1]` or
    /// `localhost`): DevTools is never driven across the network, whatever `/json/version` says.
    pub(crate) fn connect(ws_url: &str) -> Result<Self> {
        let parsed = url::Url::parse(ws_url).map_err(|e| BrowserError::InvalidUrl {
            url: ws_url.to_owned(),
            reason: e.to_string(),
        })?;
        if parsed.scheme() != "ws" {
            return Err(BrowserError::UnsupportedScheme(parsed.scheme().to_owned()));
        }
        if !is_loopback_url(&parsed) {
            return Err(BrowserError::InvalidUrl {
                url: ws_url.to_owned(),
                reason:
                    "a DevTools websocket must be on this machine (127.0.0.1, [::1] or localhost)"
                        .to_owned(),
            });
        }
        let addr = parsed
            .socket_addrs(|| None)
            .ok()
            .and_then(|a| a.into_iter().find(|a| a.ip().is_loopback()))
            .ok_or_else(|| BrowserError::InvalidUrl {
                url: ws_url.to_owned(),
                reason: "it names no reachable loopback address".to_owned(),
            })?;
        let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
            .map_err(|e| BrowserError::io(format!("could not connect to {ws_url}"), e))?;
        stream
            .set_read_timeout(Some(CONNECT_TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(CONNECT_TIMEOUT)))
            .and_then(|()| stream.set_nodelay(true))
            .map_err(|e| BrowserError::io("could not configure the DevTools socket", e))?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_MESSAGE_BYTES));
        let (ws, _) = tungstenite::client::client_with_config(ws_url, stream, Some(config))
            .map_err(|e| BrowserError::WebSocket(format!("handshake with {ws_url} failed: {e}")))?;
        Ok(Self { ws, next_id: 1 })
    }

    /// Sends `method` and waits up to `timeout` for its answer, handing every event that arrives meanwhile to
    /// `sink`. Answers to earlier calls that timed out are dropped.
    pub(crate) fn call(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
        sink: &mut dyn FnMut(Event),
    ) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send_text(encode_request(id, method, &params))?;
        let deadline = Instant::now() + timeout;
        loop {
            match self.read_until(deadline)? {
                None => {
                    return Err(BrowserError::Timeout {
                        method: method.to_owned(),
                        timeout,
                    });
                }
                Some(Incoming::Event(event)) => sink(event),
                Some(Incoming::Response { id: got, result }) if got == id => {
                    return result.map_err(|(code, message)| BrowserError::Protocol {
                        method: method.to_owned(),
                        code,
                        message,
                    });
                }
                Some(Incoming::Response { .. }) => {}
            }
        }
    }

    /// Hands events to `sink` until it returns `true` (it is done waiting) or `deadline` passes. Returns
    /// whether `sink` said it was done. Events already waiting at the deadline are still read, for at most
    /// [`DRAIN_GRACE`] more, so a short pump of a busy page cannot run forever.
    pub(crate) fn pump(
        &mut self,
        deadline: Instant,
        sink: &mut dyn FnMut(Event) -> bool,
    ) -> Result<bool> {
        while let Some(incoming) = self.read_until(deadline)? {
            if let Incoming::Event(event) = incoming
                && sink(event)
            {
                return Ok(true);
            }
            if Instant::now() >= deadline + DRAIN_GRACE {
                break;
            }
        }
        Ok(false)
    }

    /// Closes the websocket politely; errors are ignored because the other side may already be gone.
    pub(crate) fn close(&mut self) {
        let _ = self.ws.close(None);
        let _ = self.ws.flush();
    }

    fn send_text(&mut self, text: String) -> Result<()> {
        self.ws.send(Message::text(text)).map_err(ws_error)
    }

    /// The next message, or `None` once `deadline` passes without one.
    fn read_until(&mut self, deadline: Instant) -> Result<Option<Incoming>> {
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            // A zero read timeout means "block forever", so always wait at least a millisecond.
            let wait = left.max(Duration::from_millis(1));
            self.ws
                .get_mut()
                .set_read_timeout(Some(wait))
                .map_err(|e| BrowserError::io("could not set the DevTools read timeout", e))?;
            match self.ws.read() {
                Ok(Message::Text(text)) => return decode(text.as_str()).map(Some),
                Ok(Message::Binary(bytes)) => {
                    let text = std::str::from_utf8(&bytes)
                        .map_err(|_| BrowserError::BadMessage("binary, not UTF-8".to_owned()))?;
                    return decode(text).map(Some);
                }
                Ok(Message::Close(_)) => return Err(BrowserError::Closed),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    if left.is_zero() || Instant::now() >= deadline {
                        return Ok(None);
                    }
                }
                Err(e) => return Err(ws_error(e)),
            }
        }
    }
}

/// Whether `url`'s host is the machine itself.
pub(crate) fn is_loopback_url(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

fn ws_error(e: tungstenite::Error) -> BrowserError {
    match e {
        tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed => {
            BrowserError::Closed
        }
        other => BrowserError::WebSocket(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_carry_their_id_method_and_params() {
        let text = encode_request(7, "Page.navigate", &json!({ "url": "http://localhost/" }));
        let value: Value = serde_json::from_str(&text).unwrap_or_default();
        assert_eq!(value["id"], 7);
        assert_eq!(value["method"], "Page.navigate");
        assert_eq!(value["params"]["url"], "http://localhost/");
        let bare: Value = serde_json::from_str(&encode_request(8, "Page.enable", &Value::Null))
            .unwrap_or_default();
        assert_eq!(bare["params"], json!({}));
    }

    #[test]
    fn answers_errors_and_events_are_told_apart() {
        assert_eq!(
            decode(r#"{"id":3,"result":{"frameId":"F"}}"#).ok(),
            Some(Incoming::Response {
                id: 3,
                result: Ok(json!({ "frameId": "F" }))
            })
        );
        assert_eq!(
            decode(r#"{"id":4,"error":{"code":-32601,"message":"'X.y' wasn't found"}}"#).ok(),
            Some(Incoming::Response {
                id: 4,
                result: Err((-32601, "'X.y' wasn't found".to_owned()))
            })
        );
        assert_eq!(
            decode(r#"{"method":"Page.loadEventFired","params":{"timestamp":1.5}}"#).ok(),
            Some(Incoming::Event(Event {
                method: "Page.loadEventFired".to_owned(),
                params: json!({ "timestamp": 1.5 })
            }))
        );
        assert_eq!(
            decode(r#"{"id":5}"#).ok(),
            Some(Incoming::Response {
                id: 5,
                result: Ok(Value::Null)
            })
        );
    }

    #[test]
    fn only_loopback_websockets_are_connected() {
        for url in [
            "ws://192.168.1.20:9222/devtools/browser/x",
            "ws://example.com:9222/devtools/browser/x",
            "ws://localhost.evil.com:9222/devtools/browser/x",
        ] {
            assert!(
                matches!(
                    Connection::connect(url),
                    Err(BrowserError::InvalidUrl { .. })
                ),
                "{url}"
            );
        }
        let ok = |u: &str| url::Url::parse(u).is_ok_and(|u| is_loopback_url(&u));
        assert!(ok("ws://127.0.0.1:1/devtools/browser/x"));
        assert!(ok("ws://[::1]:1/devtools/browser/x"));
        assert!(ok("ws://LOCALHOST:1/devtools/browser/x"));
        assert!(matches!(
            Connection::connect("wss://127.0.0.1:1/x"),
            Err(BrowserError::UnsupportedScheme(_))
        ));
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(decode("not json").is_err());
        assert!(decode(r#"{"id":"x"}"#).is_err());
        assert!(decode(r#"{"params":{}}"#).is_err());
    }
}
