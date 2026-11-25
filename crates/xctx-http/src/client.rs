//! Sending a request: blocking, on whatever thread calls it (the app calls it from a background thread), to an
//! allowed host only, with a timeout and a cap on the body kept.

use std::io::Read;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ureq::http;
use url::Url;

use crate::error::{HttpError, Result};
use crate::exchange::{Exchange, Failure, FailureKind, Outcome, Request, Response};

/// The names of the machine itself: [`SendOptions::allow_local_port`] allows each of them on one port.
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
    /// Where requests and redirects may go. Each entry is one of:
    ///
    /// - `scheme://host:port` (or `scheme://host`, meaning the scheme's default port): that origin only;
    /// - `host:port` (`[::1]:3000` for IPv6): that host on that port, `http` or `https`;
    /// - `host` (`::1` or `[::1]` for IPv6): that host on every port. Meant for remote API hosts; use
    ///   [`SendOptions::allow_local_port`] for the machine itself, so other local services (other spaces'
    ///   apps) stay out of reach.
    ///
    /// Hosts compare without regard to case. Empty allows nothing. Add entries with the `allow_*` methods,
    /// which normalise them.
    pub allowed_hosts: Vec<String>,
    /// How many redirects to follow; `0` returns the redirect response itself.
    pub max_redirects: u32,
}

impl Default for SendOptions {
    /// The defaults above, allowing nothing: add what a space may reach with [`SendOptions::allow_local_port`],
    /// [`SendOptions::allow_url_origin`] or [`SendOptions::allow_host`].
    fn default() -> Self {
        SendOptions {
            timeout: DEFAULT_TIMEOUT,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            allowed_hosts: Vec::new(),
            max_redirects: DEFAULT_MAX_REDIRECTS,
        }
    }
}

impl SendOptions {
    /// The defaults, allowing the origin (scheme, host and port) of each of `base_urls` (a space's configured
    /// API base URLs) and nothing else. Fails when one of them is not a valid `http` or `https` URL with a host.
    pub fn for_base_urls<'a>(base_urls: impl IntoIterator<Item = &'a str>) -> Result<SendOptions> {
        let mut options = SendOptions::default();
        for base in base_urls {
            options.allow_url_origin(base)?;
        }
        Ok(options)
    }

    /// Allow `host` on every port and scheme, unless it is already allowed. A `host:port` or
    /// `scheme://host:port` allows only that (see [`SendOptions::allowed_hosts`]).
    pub fn allow_host(&mut self, host: &str) {
        if let Some(entry) = Entry::parse(host) {
            self.push(entry);
        }
    }

    /// Allow the origin of `url`: its scheme, host and port (the scheme's default port when it names none).
    /// Fails when `url` is not a valid `http` or `https` URL with a host.
    pub fn allow_url_origin(&mut self, url: &str) -> Result<()> {
        let parsed = parse_url(url)?;
        let (scheme, host, port) = origin(&parsed, url)?;
        self.push(Entry {
            scheme: Some(scheme),
            host,
            port: Some(port),
        });
        Ok(())
    }

    /// Allow the machine itself ([`LOCAL_HOSTS`]) on `port` only, over `http` or `https`: a space's own app
    /// (`localhost:$PORT`).
    pub fn allow_local_port(&mut self, port: u16) {
        for host in LOCAL_HOSTS {
            self.push(Entry {
                scheme: None,
                host: (*host).to_owned(),
                port: Some(port),
            });
        }
    }

    fn push(&mut self, entry: Entry) {
        let text = entry.to_string();
        if !self.allowed_hosts.contains(&text) {
            self.allowed_hosts.push(text);
        }
    }

    fn entries(&self) -> impl Iterator<Item = Entry> + '_ {
        self.allowed_hosts.iter().filter_map(|h| Entry::parse(h))
    }

    /// True when some entry names `host`, on any port. [`send`] still checks the scheme and port of every URL,
    /// so use [`SendOptions::allows_url`] to ask about a whole URL.
    pub fn is_allowed(&self, host: &str) -> bool {
        let host = normalize_host(host);
        self.entries().any(|e| e.host == host)
    }

    /// True when requests may go to `url`: an `http` or `https` URL whose scheme, host and port an entry allows.
    pub fn allows_url(&self, url: &str) -> bool {
        parse_url(url)
            .ok()
            .and_then(|u| origin(&u, url).ok())
            .is_some_and(|(scheme, host, port)| self.allows(&scheme, &host, port))
    }

    fn allows(&self, scheme: &str, host: &str, port: u16) -> bool {
        self.entries().any(|e| e.matches(scheme, host, port))
    }
}

/// One parsed entry of [`SendOptions::allowed_hosts`].
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    scheme: Option<String>,
    host: String,
    port: Option<u16>,
}

impl Entry {
    fn parse(raw: &str) -> Option<Entry> {
        let raw = raw.trim();
        if raw.contains("://") {
            let url = Url::parse(raw).ok()?;
            if !matches!(url.scheme(), "http" | "https") {
                return None;
            }
            let (scheme, host, port) = origin(&url, raw).ok()?;
            return Some(Entry {
                scheme: Some(scheme),
                host,
                port: Some(port),
            });
        }
        let (host, port) = if let Some(rest) = raw.strip_prefix('[') {
            let (host, after) = rest.split_once(']')?;
            let port = match after {
                "" => None,
                p => Some(p.strip_prefix(':')?.parse::<u16>().ok()?),
            };
            (host, port)
        } else if raw.matches(':').count() == 1 {
            let (host, port) = raw.split_once(':')?;
            (host, Some(port.parse::<u16>().ok()?))
        } else {
            (raw, None)
        };
        let host = normalize_host(host);
        if host.is_empty() {
            return None;
        }
        Some(Entry {
            scheme: None,
            host,
            port,
        })
    }

    fn matches(&self, scheme: &str, host: &str, port: u16) -> bool {
        self.host == host
            && self.scheme.as_deref().is_none_or(|s| s == scheme)
            && self.port.is_none_or(|p| p == port)
    }
}

impl std::fmt::Display for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        match (&self.scheme, self.port) {
            (Some(scheme), Some(port)) => write!(f, "{scheme}://{host}:{port}"),
            (Some(scheme), None) => write!(f, "{scheme}://{host}"),
            (None, Some(port)) => write!(f, "{host}:{port}"),
            // A bare IPv6 address is written without brackets, so it cannot be read as `host:port`.
            (None, None) => f.write_str(&self.host),
        }
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
        .trim_end_matches('.')
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

/// The scheme, normalised host and port (explicit or the scheme's default) of an `http` or `https` URL.
fn origin(url: &Url, raw: &str) -> Result<(String, String, u16)> {
    let host = url_host(url, raw)?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| HttpError::InvalidUrl {
            url: raw.to_owned(),
            reason: "it has no port".to_owned(),
        })?;
    Ok((url.scheme().to_owned(), host, port))
}

/// True when `a` and `b` have the same scheme, host and port: a redirect between them keeps credentials.
fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str().map(normalize_host) == b.host_str().map(normalize_host)
        && a.port_or_known_default() == b.port_or_known_default()
}

fn host_port(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn check_allowed(url: &Url, raw: &str, options: &SendOptions) -> Result<()> {
    let (scheme, host, port) = origin(url, raw)?;
    if options.allows(&scheme, &host, port) {
        return Ok(());
    }
    let allowed = if options.allowed_hosts.is_empty() {
        "no host".to_owned()
    } else {
        options.allowed_hosts.join(", ")
    };
    Err(HttpError::HostNotAllowed {
        host: host_port(&host, port),
        allowed,
    })
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
/// Fails, without sending anything, when the URL is not `http` or `https`, its scheme, host and port are not
/// allowed by `options.allowed_hosts`, or the method or a header is malformed. Once sent, every result is an
/// [`Exchange`]: a response of any status, or a [`Failure`] for a timeout, a refused connection, or a redirect
/// to an origin that is not allowed (which is never followed). A redirect to another origin (any change of
/// scheme, host or port) drops the sensitive headers ([`crate::is_sensitive_header`]), and a `307` or `308`
/// to another origin with a body is refused rather than resending the body there. `301`, `302` and `303`
/// continue as a `GET` without the body. The exchange's id is `0` until it is recorded
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
                let cross_origin = !same_origin(&url, &next);
                if cross_origin && matches!(status, 307 | 308) && body.is_some() {
                    break Outcome::Failed(Failure {
                        kind: FailureKind::RedirectRefused,
                        message: format!(
                            "a {status} redirect to {} refused: it would resend the body to another origin",
                            origin_text(&next)
                        ),
                        elapsed_ms: elapsed_ms(start),
                    });
                }
                if cross_origin {
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
    let allowed = origin(&next, location)
        .map(|(scheme, host, port)| options.allows(&scheme, &host, port))
        .unwrap_or(false);
    if !allowed {
        return Err(format!(
            "redirect to {} refused: it is not an allowed host",
            origin_text(&next)
        ));
    }
    Ok(next)
}

/// `host:port` of `url`, for messages.
fn origin_text(url: &Url) -> String {
    let host = url.host_str().map(normalize_host).unwrap_or_default();
    match url.port_or_known_default() {
        Some(port) => host_port(&host, port),
        None => host,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn url(raw: &str) -> Url {
        Url::parse(raw).unwrap_or_else(|e| panic!("{raw}: {e}"))
    }

    #[test]
    fn origins_differ_by_scheme_host_or_port() {
        assert!(same_origin(
            &url("https://a.example/x"),
            &url("https://A.example:443/y")
        ));
        assert!(!same_origin(
            &url("https://a.example/"),
            &url("http://a.example/")
        ));
        assert!(!same_origin(
            &url("http://a.example/"),
            &url("https://a.example/")
        ));
        assert!(!same_origin(
            &url("http://127.0.0.1:3000/"),
            &url("http://127.0.0.1:3001/")
        ));
        assert!(!same_origin(
            &url("http://a.example/"),
            &url("http://b.example/")
        ));
    }

    #[test]
    fn entries_read_hosts_ports_and_origins() {
        let mut options = SendOptions::default();
        assert!(!options.allows_url("http://localhost:3000/"));
        options.allow_local_port(3000);
        assert!(options.allows_url("http://localhost:3000/"));
        assert!(options.allows_url("https://127.0.0.1:3000/"));
        assert!(options.allows_url("http://[::1]:3000/x"));
        assert!(!options.allows_url("http://localhost:3001/"));
        assert!(!options.allows_url("http://localhost/"));

        assert!(
            options
                .allow_url_origin("https://Api.Example:8443/v1")
                .is_ok()
        );
        assert!(options.allows_url("https://api.example:8443/other"));
        assert!(!options.allows_url("http://api.example:8443/"));
        assert!(!options.allows_url("https://api.example/"));
        assert!(options.allow_url_origin("http://plain.example/").is_ok());
        assert!(options.allows_url("http://plain.example:80/"));
        assert!(!options.allows_url("http://plain.example:8080/"));

        options.allow_host("Any.Example");
        assert!(options.allows_url("http://any.example:1234/"));
        options.allow_host("[fe80::1]");
        assert!(options.allows_url("http://[fe80::1]:9/"));
        options.allow_host("[fe80::2]:7");
        assert!(options.allows_url("http://[fe80::2]:7/"));
        assert!(!options.allows_url("http://[fe80::2]:8/"));
        let before = options.allowed_hosts.len();
        options.allow_host("any.example");
        options.allow_local_port(3000);
        assert_eq!(options.allowed_hosts.len(), before);
        assert!(options.is_allowed("API.example"));
        assert!(!options.is_allowed("example"));
    }
}
