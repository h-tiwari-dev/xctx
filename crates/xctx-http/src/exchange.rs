//! Requests, responses and the exchanges that pair them, and masking them before they are shown.

use serde::{Deserialize, Serialize};
use xctx_core::Redactor;
use xctx_core::redact::mask;

/// Header names whose values are always masked whole by [`Exchange::masked`], compared without regard to case.
/// Names ending in one of [`SENSITIVE_HEADER_SUFFIXES`] are masked too.
pub const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "x-auth-token",
    "x-csrf-token",
    "x-goog-api-key",
    "x-amz-security-token",
    "private-token",
];

/// Endings of header names that are masked like [`SENSITIVE_HEADERS`] (`x-gitlab-token`, `x-signing-key`,
/// `x-client-secret`), compared without regard to case.
pub const SENSITIVE_HEADER_SUFFIXES: &[&str] = &["-token", "-key", "-secret"];

/// True when `name` is one of [`SENSITIVE_HEADERS`] or ends in one of [`SENSITIVE_HEADER_SUFFIXES`].
pub fn is_sensitive_header(name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    SENSITIVE_HEADERS.iter().any(|h| *h == name)
        || SENSITIVE_HEADER_SUFFIXES.iter().any(|s| name.ends_with(s))
}

/// Query and fragment keys whose values [`mask_url`] masks: any key containing one of these, compared without
/// regard to case (`token`, `access_token`, `api_key`, `client_secret`, `sig`, `X-Amz-Signature`, `auth`).
pub const SENSITIVE_QUERY_KEYS: &[&str] = &["token", "key", "secret", "password", "sig", "auth"];

/// True when the value of query key `key` is masked by [`mask_url`].
pub fn is_sensitive_query_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    SENSITIVE_QUERY_KEYS.iter().any(|k| key.contains(k))
}

/// True when `value` is a mask written by [`Exchange::masked`] or [`Exchange::masked_for_disk`]
/// (`[REDACTED:<name>]`), so the real value is gone.
pub fn is_masked_value(value: &str) -> bool {
    let value = value.trim();
    value.starts_with("[REDACTED:") && value.ends_with(']')
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

    /// A copy safe to show agents and the pane: the values of sensitive headers ([`is_sensitive_header`]) are
    /// replaced whole with `[REDACTED:<header name>]`, credentials in the URL are masked as [`mask_url`] says,
    /// and every secret `redactor` knows is masked in the URL, the other headers, both bodies and the failure
    /// message.
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

impl Exchange {
    /// A copy to write to disk ([`crate::save`] uses it): the sensitive request and response headers and the
    /// URL's credentials masked as in [`Exchange::masked`], everything else as it is. The raw values stay only
    /// in memory; an exchange loaded back cannot be replayed with them (see [`Request::masked_parts`]).
    pub fn masked_for_disk(&self) -> Exchange {
        let none = Redactor::default();
        let mut out = self.clone();
        out.request.url = mask_url(&self.request.url, &none);
        out.request.headers = mask_headers(&self.request.headers, &none);
        if let Outcome::Response(r) = &mut out.outcome {
            r.headers = mask_headers(&r.headers, &none);
        }
        out
    }
}

impl Request {
    /// What of this request was masked when it was saved, so it cannot be sent as it was: the names of headers
    /// whose value is a mask, and `url` when the URL holds one. Empty for a request that never left memory or
    /// had nothing sensitive.
    pub fn masked_parts(&self) -> Vec<String> {
        let mut parts: Vec<String> = self
            .headers
            .iter()
            .filter(|(_, v)| is_masked_value(v))
            .map(|(n, _)| n.clone())
            .collect();
        if self.url.contains("[REDACTED:") || self.url.contains("%5BREDACTED") {
            parts.push("url".to_owned());
        }
        parts
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

/// `raw` with its credentials masked, then every secret `redactor` knows: the password of the user part becomes
/// `[REDACTED:password]`; the user name becomes `[REDACTED:username]` when there is no password (a token used
/// as the user, `https://ghp_x@host`) or when it looks like a token itself; and the value of every query or
/// fragment key [`is_sensitive_query_key`] matches becomes `[REDACTED:<key>]`. The rest of the URL is kept as
/// written.
pub fn mask_url(raw: &str, redactor: &Redactor) -> String {
    let masked = mask_url_credentials(raw);
    redactor.redact(&masked).into_owned()
}

fn mask_url_credentials(raw: &str) -> String {
    let Some(scheme_end) = raw.find("://") else {
        return mask_params_in(raw);
    };
    let authority_start = scheme_end + 3;
    let rest = &raw[authority_start..];
    let authority_len = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_len];
    let tail = &rest[authority_len..];
    let authority = match authority.rfind('@') {
        Some(at) => {
            let (userinfo, host) = (&authority[..at], &authority[at..]);
            let userinfo = match userinfo.split_once(':') {
                Some((user, _password)) => {
                    let user = if looks_like_token(user) {
                        mask("username")
                    } else {
                        user.to_owned()
                    };
                    format!("{user}:{}", mask("password"))
                }
                None if userinfo.is_empty() => String::new(),
                None => mask("username"),
            };
            format!("{userinfo}{host}")
        }
        None => authority.to_owned(),
    };
    format!(
        "{}{authority}{}",
        &raw[..authority_start],
        mask_params_in(tail)
    )
}

/// `rest` (path, query and fragment) with sensitive query and fragment values masked.
fn mask_params_in(rest: &str) -> String {
    let (before_fragment, fragment) = match rest.split_once('#') {
        Some((a, f)) => (a, Some(f)),
        None => (rest, None),
    };
    let mut out = match before_fragment.split_once('?') {
        Some((path, query)) => format!("{path}?{}", mask_pairs(query)),
        None => before_fragment.to_owned(),
    };
    if let Some(fragment) = fragment {
        out.push('#');
        if fragment.contains('=') {
            out.push_str(&mask_pairs(fragment));
        } else {
            out.push_str(fragment);
        }
    }
    out
}

fn mask_pairs(pairs: &str) -> String {
    pairs
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((key, value)) if !value.is_empty() && is_sensitive_query_key(&decode(key)) => {
                format!("{key}={}", mask(&decode(key)))
            }
            _ => pair.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn decode(text: &str) -> String {
    url::form_urlencoded::parse(format!("k={text}").as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| text.to_owned())
}

/// True for a user name that looks like a token rather than a person's name: long, and letters mixed with
/// digits or with `_` and `-`.
fn looks_like_token(user: &str) -> bool {
    let user = decode(user);
    if user.chars().count() < 16 {
        return false;
    }
    let digits = user.chars().any(|c| c.is_ascii_digit());
    let letters = user.chars().any(|c| c.is_ascii_alphabetic());
    let marks = user.chars().any(|c| matches!(c, '_' | '-' | '.'));
    letters && (digits || marks) && !user.contains(' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare(raw: &str) -> String {
        mask_url(raw, &Redactor::default())
    }

    #[test]
    fn credentials_in_the_user_part_are_masked() {
        assert_eq!(
            bare("https://ghp_abcdefghijklmnop1234@github.com/x"),
            "https://[REDACTED:username]@github.com/x"
        );
        assert_eq!(
            bare("https://alice:hunter22@example.com/"),
            "https://alice:[REDACTED:password]@example.com/"
        );
        assert_eq!(
            bare("https://x-oauth-basic-0123456789abc:pw@example.com/"),
            "https://[REDACTED:username]:[REDACTED:password]@example.com/"
        );
        assert_eq!(bare("http://example.com/a@b"), "http://example.com/a@b");
    }

    #[test]
    fn token_like_query_and_fragment_values_are_masked() {
        assert_eq!(
            bare("http://h/x?token=abc&page=2&API_KEY=k1&client_secret=s&sig=zz&Auth=a&q="),
            "http://h/x?token=[REDACTED:token]&page=2&API_KEY=[REDACTED:API_KEY]\
             &client_secret=[REDACTED:client_secret]&sig=[REDACTED:sig]&Auth=[REDACTED:Auth]&q="
        );
        assert_eq!(
            bare("http://h/cb#access_token=t0k&state=1"),
            "http://h/cb#access_token=[REDACTED:access_token]&state=1"
        );
        assert_eq!(
            bare("http://h/x?password=p#top"),
            "http://h/x?password=[REDACTED:password]#top"
        );
        assert_eq!(bare("http://h/x?a=1#top"), "http://h/x?a=1#top");
    }

    #[test]
    fn more_credential_headers_are_sensitive() {
        for name in [
            "Authorization",
            "X-Auth-Token",
            "x-csrf-token",
            "X-Goog-Api-Key",
            "x-amz-security-token",
            "Private-Token",
            "X-Gitlab-Token",
            "x-signing-key",
            "X-Client-Secret",
        ] {
            assert!(is_sensitive_header(name), "{name}");
        }
        for name in ["content-type", "x-trace", "accept", "keyboard"] {
            assert!(!is_sensitive_header(name), "{name}");
        }
    }

    #[test]
    fn disk_copies_mask_headers_and_list_what_is_gone() {
        let exchange = Exchange {
            id: 1,
            request: Request::new("GET", "http://u:pw@h/x?token=t&a=1")
                .header("Authorization", "Bearer abc")
                .header("Accept", "text/plain"),
            outcome: Outcome::Response(Response {
                status: 200,
                headers: vec![("set-cookie".into(), "s=1".into())],
                body: "body".into(),
                truncated: false,
                elapsed_ms: 1,
            }),
            at: 0,
        };
        assert!(exchange.request.masked_parts().is_empty());
        let disk = exchange.masked_for_disk();
        assert_eq!(disk.request.headers[0].1, "[REDACTED:Authorization]");
        assert_eq!(disk.request.headers[1].1, "text/plain");
        assert_eq!(
            disk.response().and_then(|r| r.header("set-cookie")),
            Some("[REDACTED:set-cookie]")
        );
        assert_eq!(
            disk.request.masked_parts(),
            vec!["Authorization".to_owned(), "url".to_owned()]
        );
        assert_eq!(disk.response().map(|r| r.body.as_str()), Some("body"));
    }
}
