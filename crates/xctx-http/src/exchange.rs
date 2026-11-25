//! Requests, responses and the exchanges that pair them, and masking them before they are shown.

use serde::{Deserialize, Serialize};
use xctx_core::Redactor;
use xctx_core::redact::mask;

/// Header names whose values are always masked whole by [`Exchange::masked`], compared without regard to case.
pub const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
];

/// True when `name` is one of [`SENSITIVE_HEADERS`].
pub fn is_sensitive_header(name: &str) -> bool {
    SENSITIVE_HEADERS
        .iter()
        .any(|h| h.eq_ignore_ascii_case(name))
}

/// A request to send: what the user typed in the pane or an agent passed to `http.send`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// The method, such as `GET` or `POST`. Sent upper-cased.
    pub method: String,
    /// The absolute `http` or `https` URL.
    pub url: String,
    /// Headers in the order they are sent. A name may repeat.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// The body, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

impl Request {
    /// A request with no headers and no body.
    pub fn new(method: impl Into<String>, url: impl Into<String>) -> Request {
        Request {
            method: method.into(),
            url: url.into(),
            headers: Vec::new(),
            body: None,
        }
    }

    /// This request with a header added.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Request {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// This request with `body` as its body.
    pub fn body(mut self, body: impl Into<String>) -> Request {
        self.body = Some(body.into());
        self
    }

    /// A short label such as `POST /checkout`, for lists and summaries.
    pub fn label(&self) -> String {
        let path = url::Url::parse(&self.url)
            .map(|u| u.path().to_owned())
            .unwrap_or_else(|_| self.url.clone());
        format!("{} {}", self.method.to_ascii_uppercase(), path)
    }
}

/// The answer to a request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    /// The status code.
    pub status: u16,
    /// The headers as received, names lower-cased.
    pub headers: Vec<(String, String)>,
    /// The body, decoded as UTF-8 with invalid bytes replaced, and cut at the send's `max_body_bytes`.
    pub body: String,
    /// True when the body was longer than `max_body_bytes` and was cut.
    #[serde(default)]
    pub truncated: bool,
    /// How long the whole send took, redirects included, in milliseconds.
    pub elapsed_ms: u64,
}

impl Response {
    /// The first value of header `name`, compared without regard to case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Why a request that was sent got no response.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// It took longer than the send's timeout.
    Timeout,
    /// The host could not be resolved or reached.
    Connect,
    /// The server redirected to a host that is not allowed, or to something that is not `http` or `https`.
    RedirectRefused,
    /// The server redirected more times than the send follows.
    TooManyRedirects,
    /// The TLS handshake failed.
    Tls,
    /// Anything else: a malformed response, a dropped connection.
    Other,
}

/// A request that was sent but got no response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    /// What went wrong.
    pub kind: FailureKind,
    /// A message for the user.
    pub message: String,
    /// How long it took to fail, in milliseconds.
    pub elapsed_ms: u64,
}

/// What came back for a request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// A response, whatever its status.
    Response(Response),
    /// No response.
    Failed(Failure),
}

/// A request and what came back, as kept in a space's history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exchange {
    /// Its number in the space's history, rising and never reused. `0` until [`crate::HttpHistory::record`]
    /// gives it one.
    pub id: u64,
    /// What was sent.
    pub request: Request,
    /// What came back.
    pub outcome: Outcome,
    /// When it was sent, in seconds since the Unix epoch.
    pub at: u64,
}

impl Exchange {
    /// The response, if there was one.
    pub fn response(&self) -> Option<&Response> {
        match &self.outcome {
            Outcome::Response(r) => Some(r),
            Outcome::Failed(_) => None,
        }
    }

    /// The failure, if there was no response.
    pub fn failure(&self) -> Option<&Failure> {
        match &self.outcome {
            Outcome::Response(_) => None,
            Outcome::Failed(f) => Some(f),
        }
    }

    /// The response's status, if there was a response.
    pub fn status(&self) -> Option<u16> {
        self.response().map(|r| r.status)
    }

    /// How long the send took, whether or not it got a response.
    pub fn elapsed_ms(&self) -> u64 {
        match &self.outcome {
            Outcome::Response(r) => r.elapsed_ms,
            Outcome::Failed(f) => f.elapsed_ms,
        }
    }

    /// A copy safe to show agents and the pane: the values of [`SENSITIVE_HEADERS`] are replaced whole with
    /// `[REDACTED:<header name>]`, a password in the URL is replaced with `[REDACTED:password]`, and every
    /// secret `redactor` knows is masked in the URL, the other headers, both bodies and the failure message.
    pub fn masked(&self, redactor: &Redactor) -> Exchange {
        let request = Request {
            method: self.request.method.clone(),
            url: mask_url(&self.request.url, redactor),
            headers: mask_headers(&self.request.headers, redactor),
            body: self
                .request
                .body
                .as_deref()
                .map(|b| redactor.redact(b).into_owned()),
        };
        let outcome = match &self.outcome {
            Outcome::Response(r) => Outcome::Response(Response {
                status: r.status,
                headers: mask_headers(&r.headers, redactor),
                body: redactor.redact(&r.body).into_owned(),
                truncated: r.truncated,
                elapsed_ms: r.elapsed_ms,
            }),
            Outcome::Failed(f) => Outcome::Failed(Failure {
                kind: f.kind,
                message: redactor.redact(&f.message).into_owned(),
                elapsed_ms: f.elapsed_ms,
            }),
        };
        Exchange {
            id: self.id,
            request,
            outcome,
            at: self.at,
        }
    }
}

fn mask_headers(headers: &[(String, String)], redactor: &Redactor) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            let value = if is_sensitive_header(name) {
                mask(name)
            } else {
                redactor.redact(value).into_owned()
            };
            (name.clone(), value)
        })
        .collect()
}

fn mask_url(raw: &str, redactor: &Redactor) -> String {
    let password = url::Url::parse(raw)
        .ok()
        .and_then(|u| u.password().map(str::to_owned));
    let without_password = match password {
        Some(pw) => {
            let found = format!(":{pw}@");
            if raw.contains(&found) {
                raw.replacen(&found, &format!(":{}@", mask("password")), 1)
            } else {
                // The URL spells the password differently from its parsed form: drop the whole user part.
                match url::Url::parse(raw) {
                    Ok(mut u) => {
                        let _ = u.set_password(None);
                        let _ = u.set_username("");
                        u.to_string()
                    }
                    Err(_) => raw.to_owned(),
                }
            }
        }
        None => raw.to_owned(),
    };
    redactor.redact(&without_password).into_owned()
}
