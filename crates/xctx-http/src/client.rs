//! Sending a request: blocking, on whatever thread calls it (the app calls it from a background thread), to an
//! allowed host only, with a timeout and a cap on the body kept.

use std::io::Read;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ureq::http;
use url::Url;

use crate::error::{HttpError, Result};
use crate::exchange::{Exchange, Failure, FailureKind, Outcome, Request, Response};

/// The hosts every space may send to: the machine itself.
pub const LOCAL_HOSTS: &[&str] = &["localhost", "127.0.0.1", "::1"];

/// The timeout [`SendOptions::default`] uses.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// The body cap [`SendOptions::default`] uses: 1 MiB.
pub const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;

/// How many redirects [`SendOptions::default`] follows.
pub const DEFAULT_MAX_REDIRECTS: u32 = 5;

/// How a request is sent and what it may reach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendOptions {
    /// The longest one send may take, connecting and reading the body included (per redirect hop).
    pub timeout: Duration,
    /// The most bytes of a response body kept; the rest is not read and the response is marked truncated.
    pub max_body_bytes: usize,
    /// The hosts requests and redirects may go to, compared without regard to case and port. IPv6 addresses
    /// are written without brackets (`::1`). Empty allows nothing.
    pub allowed_hosts: Vec<String>,
    /// How many redirects to follow; `0` returns the redirect response itself.
    pub max_redirects: u32,
}

impl Default for SendOptions {
    /// The defaults above, allowing only [`LOCAL_HOSTS`].
    fn default() -> Self {
        SendOptions {
            timeout: DEFAULT_TIMEOUT,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            allowed_hosts: LOCAL_HOSTS.iter().map(|h| (*h).to_owned()).collect(),
            max_redirects: DEFAULT_MAX_REDIRECTS,
        }
    }
}

impl SendOptions {
    /// The defaults, also allowing the host of each of `base_urls` (a space's configured API base URLs).
    /// Fails when one of them is not a valid `http` or `https` URL with a host.
    pub fn for_base_urls<'a>(base_urls: impl IntoIterator<Item = &'a str>) -> Result<SendOptions> {
        let mut options = SendOptions::default();
        for base in base_urls {
            let host = host_of(base)?;
            options.allow_host(&host);
        }
        Ok(options)
    }

    /// Allow `host` too, unless it is already allowed.
    pub fn allow_host(&mut self, host: &str) {
        let host = normalize_host(host);
        if !self.allowed_hosts.iter().any(|h| normalize_host(h) == host) {
            self.allowed_hosts.push(host);
        }
    }

    /// True when requests may go to `host`.
    pub fn is_allowed(&self, host: &str) -> bool {
        let host = normalize_host(host);
        self.allowed_hosts.iter().any(|h| normalize_host(h) == host)
    }
}

/// The host of `raw`, lower-cased and without brackets, after checking it is an `http` or `https` URL.
pub fn host_of(raw: &str) -> Result<String> {
    let url = parse_url(raw)?;
    url_host(&url, raw)
}

fn normalize_host(host: &str) -> String {
    host.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase()
}

fn parse_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).map_err(|e| HttpError::InvalidUrl {
        url: raw.to_owned(),
        reason: e.to_string(),
    })?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        other => Err(HttpError::UnsupportedScheme(other.to_owned())),
    }
}

fn url_host(url: &Url, raw: &str) -> Result<String> {
    url.host_str()
        .map(normalize_host)
        .ok_or_else(|| HttpError::InvalidUrl {
            url: raw.to_owned(),
            reason: "it has no host".to_owned(),
        })
}

fn check_allowed(url: &Url, raw: &str, options: &SendOptions) -> Result<()> {
    let host = url_host(url, raw)?;
    if options.is_allowed(&host) {
        return Ok(());
    }
    let allowed = if options.allowed_hosts.is_empty() {
        "no host".to_owned()
    } else {
        options.allowed_hosts.join(", ")
    };
    Err(HttpError::HostNotAllowed { host, allowed })
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn elapsed_ms(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Send `request` and wait for the answer.
///
/// Fails, without sending anything, when the URL is not `http` or `https`, its host is not in
/// `options.allowed_hosts`, or the method or a header is malformed. Once sent, every result is an [`Exchange`]:
/// a response of any status, or a [`Failure`] for a timeout, a refused connection, or a redirect to a host
/// that is not allowed (which is never followed). Redirects drop sensitive headers when they change host, and
/// `301`, `302` and `303` continue as a `GET` without the body. The exchange's id is `0` until it is recorded
/// in a history. No proxy is used, whatever the environment says.
pub fn send(request: &Request, options: &SendOptions) -> Result<Exchange> {
    let at = now_secs();
    let mut url = parse_url(&request.url)?;
    check_allowed(&url, &request.url, options)?;
    let mut method = parse_method(&request.method)?;
    let mut headers = parse_headers(&request.headers)?;
    let mut body = request.body.clone();

    let agent = agent(options);
    let start = Instant::now();
    let mut redirects = 0u32;
    let outcome = loop {
        let response = match run(&agent, &method, &url, &headers, body.as_deref()) {
            Ok(r) => r,
            Err(e) => break Outcome::Failed(failure(e, start)),
        };
        let status = response.status().as_u16();
        let location = response
            .headers()
            .get(http::header::LOCATION)
            .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned());
        let is_redirect = matches!(status, 301 | 302 | 303 | 307 | 308);
        match location {
            Some(location) if is_redirect && options.max_redirects > 0 => {
                if redirects >= options.max_redirects {
                    break Outcome::Failed(Failure {
                        kind: FailureKind::TooManyRedirects,
                        message: format!("stopped after {redirects} redirects"),
                        elapsed_ms: elapsed_ms(start),
                    });
                }
                let next = match redirect_target(&url, &location, options) {
                    Ok(next) => next,
                    Err(message) => {
                        break Outcome::Failed(Failure {
                            kind: FailureKind::RedirectRefused,
                            message,
                            elapsed_ms: elapsed_ms(start),
                        });
                    }
                };
                if next.host_str() != url.host_str() {
                    headers
                        .retain(|(name, _)| !crate::exchange::is_sensitive_header(name.as_str()));
                }
                if matches!(status, 301..=303) && method != http::Method::HEAD {
                    method = http::Method::GET;
                    body = None;
                }
                url = next;
                redirects += 1;
            }
            _ => break Outcome::Response(read_response(response, options, start)),
        }
    };
    Ok(Exchange {
        id: 0,
        request: request.clone(),
        outcome,
        at,
    })
}

fn agent(options: &SendOptions) -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None)
        .allow_non_standard_methods(true)
        .timeout_global(Some(options.timeout))
        .tls_config(tls)
        .build()
        .into()
}

fn parse_method(method: &str) -> Result<http::Method> {
    let upper = method.trim().to_ascii_uppercase();
    if upper.is_empty() {
        return Err(HttpError::InvalidRequest("the method is empty".to_owned()));
    }
    http::Method::from_bytes(upper.as_bytes())
        .map_err(|_| HttpError::InvalidRequest(format!("{method:?} is not a valid method")))
}

fn parse_headers(
    headers: &[(String, String)],
) -> Result<Vec<(http::HeaderName, http::HeaderValue)>> {
    headers
        .iter()
        .map(|(name, value)| {
            let n = http::HeaderName::from_bytes(name.trim().as_bytes()).map_err(|_| {
                HttpError::InvalidRequest(format!("{name:?} is not a valid header name"))
            })?;
            let v = http::HeaderValue::from_str(value.trim()).map_err(|_| {
                HttpError::InvalidRequest(format!("the value of header {name} is not valid"))
            })?;
            Ok((n, v))
        })
        .collect()
}

fn run(
    agent: &ureq::Agent,
    method: &http::Method,
    url: &Url,
    headers: &[(http::HeaderName, http::HeaderValue)],
    body: Option<&str>,
) -> std::result::Result<http::Response<ureq::Body>, ureq::Error> {
    let mut builder = http::Request::builder()
        .method(method.clone())
        .uri(url.as_str());
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    match body {
        Some(body) => agent.run(builder.body(body.to_owned())?),
        None => agent.run(builder.body(())?),
    }
}

/// The URL a redirect from `from` to `location` goes to, or why it is refused.
fn redirect_target(
    from: &Url,
    location: &str,
    options: &SendOptions,
) -> std::result::Result<Url, String> {
    let next = from
        .join(location)
        .map_err(|e| format!("redirect to {location:?} refused: {e}"))?;
    if !matches!(next.scheme(), "http" | "https") {
        return Err(format!(
            "redirect to {location:?} refused: only http and https are followed"
        ));
    }
    let host = next.host_str().map(normalize_host).unwrap_or_default();
    if !options.is_allowed(&host) {
        return Err(format!(
            "redirect to {host} refused: it is not an allowed host"
        ));
    }
    Ok(next)
}

fn read_response(
    response: http::Response<ureq::Body>,
    options: &SendOptions,
    start: Instant,
) -> Response {
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(n, v)| {
            (
                n.as_str().to_owned(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect();
    let (_, body) = response.into_parts();
    let limit = u64::try_from(options.max_body_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut bytes = Vec::new();
    // A body that breaks off part way keeps what arrived; the status and headers are still worth showing.
    let _ = body.into_reader().take(limit).read_to_end(&mut bytes);
    let truncated = bytes.len() > options.max_body_bytes;
    bytes.truncate(options.max_body_bytes);
    Response {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
        truncated,
        elapsed_ms: elapsed_ms(start),
    }
}

fn failure(error: ureq::Error, start: Instant) -> Failure {
    let kind = match &error {
        ureq::Error::Timeout(_) => FailureKind::Timeout,
        ureq::Error::HostNotFound | ureq::Error::ConnectionFailed => FailureKind::Connect,
        ureq::Error::Io(e) if is_connect_error(e.kind()) => FailureKind::Connect,
        ureq::Error::Io(e) if e.kind() == std::io::ErrorKind::TimedOut => FailureKind::Timeout,
        ureq::Error::Tls(_) | ureq::Error::NativeTls(_) | ureq::Error::TlsRequired => {
            FailureKind::Tls
        }
        ureq::Error::TooManyRedirects => FailureKind::TooManyRedirects,
        _ => FailureKind::Other,
    };
    Failure {
        kind,
        message: error.to_string(),
        elapsed_ms: elapsed_ms(start),
    }
}

fn is_connect_error(kind: std::io::ErrorKind) -> bool {
    use std::io::ErrorKind::*;
    matches!(
        kind,
        ConnectionRefused | ConnectionReset | ConnectionAborted | NotConnected | AddrNotAvailable
    )
}
