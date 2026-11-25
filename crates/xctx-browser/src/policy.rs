//! Which pages the browser may open.

use url::{Host, Url};

use crate::error::{BrowserError, Result};

/// The hosts every policy allows by default: the machine itself.
pub const LOCAL_HOSTS: &[&str] = &["localhost", "127.0.0.1", "::1"];

/// The hosts the browser may navigate to, checked by [`crate::Page::open`] before anything is sent.
///
/// The default allows only [`LOCAL_HOSTS`], so a space's app (`localhost:$PORT`) can be opened and nothing
/// else; the app adds the hosts of a space's configured base URLs with [`UrlPolicy::allow_url_host`]. Only
/// `http` and `https` URLs pass. The check covers the URL the caller asks for; pages the site itself loads
/// (scripts, redirects, fetches) are not filtered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlPolicy {
    /// The allowed hosts, lower case, IPv6 addresses without brackets. Ports are not checked.
    pub allowed_hosts: Vec<String>,
}

impl Default for UrlPolicy {
    fn default() -> Self {
        Self {
            allowed_hosts: LOCAL_HOSTS.iter().map(|h| (*h).to_owned()).collect(),
        }
    }
}

impl UrlPolicy {
    /// A policy that allows exactly `hosts` (normalised; no local hosts unless listed).
    pub fn only<I, S>(hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut policy = Self {
            allowed_hosts: Vec::new(),
        };
        for host in hosts {
            policy.allow_host(host.as_ref());
        }
        policy
    }

    /// Also allows `host` (for example `staging.example.com` or `[::1]`).
    pub fn allow_host(&mut self, host: &str) {
        let host = normalize_host(host);
        if !host.is_empty() && !self.allowed_hosts.contains(&host) {
            self.allowed_hosts.push(host);
        }
    }

    /// Also allows the host of `url` (a space's base URL). Fails if `url` has no host.
    pub fn allow_url_host(&mut self, url: &str) -> Result<()> {
        let parsed = parse(url)?;
        let host = host_of(&parsed).ok_or_else(|| BrowserError::InvalidUrl {
            url: url.to_owned(),
            reason: "it has no host".to_owned(),
        })?;
        self.allow_host(&host);
        Ok(())
    }

    /// Whether `host` is allowed (compared normalised).
    pub fn allows_host(&self, host: &str) -> bool {
        let host = normalize_host(host);
        self.allowed_hosts.iter().any(|h| normalize_host(h) == host)
    }

    /// The parsed URL if it may be opened, or why not.
    pub fn check(&self, url: &str) -> Result<Url> {
        let parsed = parse(url)?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(BrowserError::UnsupportedScheme(parsed.scheme().to_owned()));
        }
        let host = host_of(&parsed).unwrap_or_default();
        if !self.allows_host(&host) {
            let allowed = if self.allowed_hosts.is_empty() {
                "no host".to_owned()
            } else {
                self.allowed_hosts.join(", ")
            };
            return Err(BrowserError::HostNotAllowed { host, allowed });
        }
        Ok(parsed)
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

    #[test]
    fn the_default_allows_the_machine_itself_on_any_port() {
        let policy = UrlPolicy::default();
        for url in [
            "http://localhost:3000/",
            "http://LOCALHOST/x",
            "https://127.0.0.1:8443/a?b",
            "http://[::1]:5173/",
        ] {
            assert!(policy.check(url).is_ok(), "{url}");
        }
    }

    #[test]
    fn other_hosts_are_refused_with_the_allowed_list() {
        let err = UrlPolicy::default()
            .check("https://example.com/")
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("example.com"), "{err}");
        assert!(err.contains("localhost, 127.0.0.1, ::1"), "{err}");
        assert!(matches!(
            UrlPolicy::default().check("http://localhost.evil.com/"),
            Err(BrowserError::HostNotAllowed { .. })
        ));
        assert!(matches!(
            UrlPolicy::only(Vec::<String>::new()).check("http://localhost/"),
            Err(BrowserError::HostNotAllowed { .. })
        ));
    }

    #[test]
    fn only_web_urls_pass() {
        let policy = UrlPolicy::default();
        assert!(matches!(
            policy.check("file:///etc/passwd"),
            Err(BrowserError::UnsupportedScheme(s)) if s == "file"
        ));
        assert!(matches!(
            policy.check("javascript:alert(1)"),
            Err(BrowserError::UnsupportedScheme(_))
        ));
        assert!(matches!(
            policy.check("not a url"),
            Err(BrowserError::InvalidUrl { .. })
        ));
    }

    #[test]
    fn base_url_hosts_can_be_added() {
        let mut policy = UrlPolicy::default();
        assert!(
            policy
                .allow_url_host("https://Staging.Example.com/api")
                .is_ok()
        );
        assert!(policy.check("https://staging.example.com/login").is_ok());
        assert!(policy.check("https://example.com/").is_err());
        assert!(policy.allow_url_host("mailto:x").is_err());
        policy.allow_host("[fe80::1]");
        assert!(policy.allows_host("fe80::1"));
        let before = policy.allowed_hosts.len();
        policy.allow_host("LOCALHOST");
        assert_eq!(policy.allowed_hosts.len(), before);
    }
}
