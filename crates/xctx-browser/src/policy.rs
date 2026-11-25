//! Which pages the browser may open.

use url::{Host, Url};

use crate::error::{BrowserError, Result};

/// The names of the machine itself: [`UrlPolicy::allow_local_port`] allows each of them on one port.
pub const LOCAL_HOSTS: &[&str] = &["localhost", "127.0.0.1", "::1"];

/// The origins the browser may navigate to, checked by [`crate::Page::open`] before anything is sent, and by
/// the app against [`crate::Page::current_url`] before it captures a page.
///
/// The default allows nothing. The app allows the space's app with [`UrlPolicy::allow_local_port`] (the
/// space's `PORT` on the machine itself, no other local port: other spaces' apps and Chrome's own DevTools
/// port stay out of reach) and a space's configured base URLs with [`UrlPolicy::allow_url_origin`]. Only
/// `http` and `https` URLs pass. The check covers the URL the caller asks for; pages the site itself loads
/// (scripts, redirects, fetches) are not filtered, which is why the current URL must be checked again.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UrlPolicy {
    /// The allowed origins. Each entry is `scheme://host:port` (that origin only), `host:port` (`[::1]:3000`
    /// for IPv6; that host and port, `http` or `https`) or a bare `host` (every port). Hosts are lower case,
    /// IPv6 addresses in a bare host without brackets.
    pub allowed_hosts: Vec<String>,
}

impl UrlPolicy {
    /// A policy that allows exactly `hosts`, each read as an entry of [`UrlPolicy::allowed_hosts`] (no local
    /// hosts unless listed).
    pub fn only<I, S>(hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut policy = Self::default();
        for host in hosts {
            policy.allow_host(host.as_ref());
        }
        policy
    }

    /// Also allows `host` on every port (for example `staging.example.com` or `[::1]`), or only one port or
    /// origin when it is written `host:port` or `scheme://host:port`. For the machine itself use
    /// [`UrlPolicy::allow_local_port`].
    pub fn allow_host(&mut self, host: &str) {
        if let Some(entry) = Entry::parse(host) {
            self.push(entry);
        }
    }

    /// Also allows the host of `url` on every port. Fails if `url` has no host. Prefer
    /// [`UrlPolicy::allow_url_origin`], which keeps the port.
    pub fn allow_url_host(&mut self, url: &str) -> Result<()> {
        let parsed = parse(url)?;
        let host = host_of(&parsed).ok_or_else(|| no_host(url))?;
        self.push(Entry {
            scheme: None,
            host,
            port: None,
        });
        Ok(())
    }

    /// Also allows the origin of `url` (a space's base URL): its scheme, host and port (the scheme's default
    /// when it names none). Fails unless `url` is an `http` or `https` URL with a host.
    pub fn allow_url_origin(&mut self, url: &str) -> Result<()> {
        let parsed = parse(url)?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(BrowserError::UnsupportedScheme(parsed.scheme().to_owned()));
        }
        let (scheme, host, port) = origin(&parsed).ok_or_else(|| no_host(url))?;
        self.push(Entry {
            scheme: Some(scheme),
            host,
            port: Some(port),
        });
        Ok(())
    }

    /// Also allows the machine itself ([`LOCAL_HOSTS`]) on `port` only, over `http` or `https`: a space's app
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

    /// Whether some entry names `host` (compared normalised), on at least one port. [`UrlPolicy::check`] and
    /// [`UrlPolicy::allows_url`] also check the scheme and port.
    pub fn allows_host(&self, host: &str) -> bool {
        let host = normalize_host(host);
        self.entries().any(|e| e.host == host)
    }

    /// Whether `url` may be opened, or be shown once a page is on it: [`UrlPolicy::check`] passes.
    pub fn allows_url(&self, url: &str) -> bool {
        self.check(url).is_ok()
    }

    /// The parsed URL if it may be opened, or why not.
    pub fn check(&self, url: &str) -> Result<Url> {
        let parsed = parse(url)?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(BrowserError::UnsupportedScheme(parsed.scheme().to_owned()));
        }
        let (scheme, host, port) = origin(&parsed).ok_or_else(|| no_host(url))?;
        if !self.entries().any(|e| e.matches(&scheme, &host, port)) {
            let allowed = if self.allowed_hosts.is_empty() {
                "no host".to_owned()
            } else {
                self.allowed_hosts.join(", ")
            };
            return Err(BrowserError::HostNotAllowed {
                host: host_port(&host, port),
                allowed,
            });
        }
        Ok(parsed)
    }
}

/// One parsed entry of [`UrlPolicy::allowed_hosts`].
#[derive(Debug, Clone, PartialEq, Eq)]
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
            let (scheme, host, port) = origin(&url)?;
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
        let bracketed = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        match (&self.scheme, self.port) {
            (Some(scheme), Some(port)) => write!(f, "{scheme}://{bracketed}:{port}"),
            (Some(scheme), None) => write!(f, "{scheme}://{bracketed}"),
            (None, Some(port)) => write!(f, "{bracketed}:{port}"),
            (None, None) => f.write_str(&self.host),
        }
    }
}

fn no_host(url: &str) -> BrowserError {
    BrowserError::InvalidUrl {
        url: url.to_owned(),
        reason: "it has no host".to_owned(),
    }
}

fn parse(url: &str) -> Result<Url> {
    Url::parse(url.trim()).map_err(|e| BrowserError::InvalidUrl {
        url: url.to_owned(),
        reason: e.to_string(),
    })
}

fn host_of(url: &Url) -> Option<String> {
    match url.host()? {
        Host::Domain(d) => Some(normalize_host(d)),
        Host::Ipv4(ip) => Some(ip.to_string()),
        Host::Ipv6(ip) => Some(ip.to_string()),
    }
}

/// The scheme, normalised host and port (explicit or the scheme's default) of `url`.
fn origin(url: &Url) -> Option<(String, String, u16)> {
    Some((
        url.scheme().to_owned(),
        host_of(url)?,
        url.port_or_known_default()?,
    ))
}

fn host_port(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn normalize_host(host: &str) -> String {
    let host = host.trim().trim_end_matches('.');
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    host.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(port: u16) -> UrlPolicy {
        let mut policy = UrlPolicy::default();
        policy.allow_local_port(port);
        policy
    }

    #[test]
    fn the_default_allows_nothing() {
        let policy = UrlPolicy::default();
        for url in [
            "http://localhost:3000/",
            "http://127.0.0.1/",
            "https://example.com/",
        ] {
            assert!(
                matches!(policy.check(url), Err(BrowserError::HostNotAllowed { .. })),
                "{url}"
            );
        }
    }

    #[test]
    fn a_local_port_allows_the_machine_on_that_port_only() {
        let policy = local(3000);
        for url in [
            "http://localhost:3000/",
            "http://LOCALHOST:3000/x",
            "https://127.0.0.1:3000/a?b",
            "http://[::1]:3000/",
        ] {
            assert!(policy.check(url).is_ok(), "{url}");
            assert!(policy.allows_url(url), "{url}");
        }
        for url in [
            "http://localhost:3001/",
            "http://127.0.0.1/",
            "http://127.0.0.1:9222/json/version",
            "http://[::1]:5173/",
        ] {
            assert!(!policy.allows_url(url), "{url}");
        }
        assert!(matches!(
            policy.check("http://127.0.0.1:9222/"),
            Err(BrowserError::HostNotAllowed { host, .. }) if host == "127.0.0.1:9222"
        ));
    }

    #[test]
    fn other_hosts_are_refused_with_the_allowed_list() {
        let err = local(3000)
            .check("https://example.com/")
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("example.com:443"), "{err}");
        assert!(
            err.contains("localhost:3000, 127.0.0.1:3000, [::1]:3000"),
            "{err}"
        );
        assert!(matches!(
            local(80).check("http://localhost.evil.com/"),
            Err(BrowserError::HostNotAllowed { .. })
        ));
        assert!(matches!(
            UrlPolicy::only(Vec::<String>::new()).check("http://localhost/"),
            Err(BrowserError::HostNotAllowed { .. })
        ));
    }

    #[test]
    fn only_web_urls_pass() {
        let policy = local(3000);
        assert!(matches!(
            policy.check("file:///etc/passwd"),
            Err(BrowserError::UnsupportedScheme(s)) if s == "file"
        ));
        assert!(matches!(
            policy.check("javascript:alert(1)"),
            Err(BrowserError::UnsupportedScheme(_))
        ));
        assert!(!policy.allows_url("about:blank"));
        assert!(matches!(
            policy.check("not a url"),
            Err(BrowserError::InvalidUrl { .. })
        ));
    }

    #[test]
    fn base_url_origins_can_be_added() {
        let mut policy = UrlPolicy::default();
        assert!(
            policy
                .allow_url_origin("https://Staging.Example.com/api")
                .is_ok()
        );
        assert!(policy.check("https://staging.example.com/login").is_ok());
        assert!(policy.check("https://staging.example.com:443/").is_ok());
        assert!(policy.check("http://staging.example.com/").is_err());
        assert!(policy.check("https://staging.example.com:8443/").is_err());
        assert!(policy.check("https://example.com/").is_err());
        assert!(policy.allow_url_origin("mailto:x").is_err());
        assert!(policy.allow_url_host("http://any.example:1/").is_ok());
        assert!(policy.allows_url("https://any.example:9/"));
        policy.allow_host("[fe80::1]");
        assert!(policy.allows_host("fe80::1"));
        assert!(policy.allows_url("http://[fe80::1]:7/"));
        let before = policy.allowed_hosts.len();
        policy.allow_host("FE80::1");
        assert_eq!(policy.allowed_hosts.len(), before);
    }
}
