//! Which pages the agents' `page.*` tools may open and read, which pages the user's browsing may open, and
//! what the pane's URL bar opens for what the user typed.

use url::{Host, Url};

use crate::error::{BrowserError, Result};

/// The names of the machine itself: [`UrlPolicy::allow_local_port`] allows each of them on one port.
pub const LOCAL_HOSTS: &[&str] = &["localhost", "127.0.0.1", "::1"];

/// The schemes the user's own navigation in the live browser may load: web pages, and the documents a page
/// makes itself.
const USER_SCHEMES: &[&str] = &["http", "https", "about", "data", "blob"];

/// The origins the agents' `page.*` tools may use: `page.open` refuses any other URL ([`UrlPolicy::check`])
/// and the other tools refuse to read, capture or act on a page that is anywhere else
/// ([`UrlPolicy::check_current`]), so nothing the user browses to reaches an agent.
///
/// The user's own browsing in the pane is not limited by it. The default allows nothing. The app allows the
/// space's app with [`UrlPolicy::allow_local_port`] (the space's `PORT` on the machine itself, no other local
/// port: other spaces' apps stay out of reach) and the pane's configured page with
/// [`UrlPolicy::allow_url_origin`]. Only `http` and `https` URLs pass. The check covers the URL the caller
/// asks for; pages the site itself loads (scripts, redirects, fetches) are not filtered, which is why the
/// current URL is checked again after every action.
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

    /// The page URL `current` (as the browser reports it; `None` before anything was opened) if the tools may
    /// read and act on it: an allowed URL, or a blank page. Otherwise [`BrowserError::OtherSite`], which
    /// does not say where the page is: the user's browsing stays theirs.
    pub fn check_current(&self, current: Option<&str>) -> Result<String> {
        match current.map(str::trim).filter(|u| !u.is_empty()) {
            None => Ok("about:blank".to_owned()),
            Some("about:blank") => Ok("about:blank".to_owned()),
            Some(url) if self.allows_url(url) => Ok(url.to_owned()),
            Some(_) => Err(BrowserError::OtherSite),
        }
    }
}

/// Whether the user's own navigation in the live browser (typed, clicked, or started by the page) may load
/// `url`: any `http` or `https` page, and `about:`, `data:` and `blob:` documents. Other schemes (`file:`,
/// custom app schemes) are refused with why. The agents' tools are limited further by [`UrlPolicy`].
pub fn user_may_open(url: &str) -> Result<()> {
    let parsed = parse(url)?;
    if USER_SCHEMES.contains(&parsed.scheme()) {
        Ok(())
    } else {
        Err(BrowserError::UnsupportedScheme(parsed.scheme().to_owned()))
    }
}

/// The URL the pane's URL bar opens for what the user typed: a path (`/login`) on `base` (the pane's page,
/// `http://localhost:{PORT}`), `base` itself for nothing, a URL with a scheme as it is, and a bare host with
/// `http://` for the machine itself (`localhost:3000/x`) and `https://` for anything else (`google.com`).
pub fn typed_url(typed: &str, base: &str) -> String {
    let typed = typed.trim();
    if typed.is_empty() {
        return base.to_owned();
    }
    if typed.starts_with('/') {
        return Url::parse(base)
            .and_then(|b| b.join(typed))
            .map(String::from)
            .unwrap_or_else(|_| format!("{}{typed}", base.trim_end_matches('/')));
    }
    let lower = typed.to_ascii_lowercase();
    if lower.contains("://")
        || ["about:", "data:", "blob:"]
            .iter()
            .any(|s| lower.starts_with(s))
    {
        return typed.to_owned();
    }
    let host = lower.split(['/', '?', '#']).next().unwrap_or_default();
    let host = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => host.split(':').next().unwrap_or_default(),
    };
    if LOCAL_HOSTS.contains(&host) {
        format!("http://{typed}")
    } else {
        format!("https://{typed}")
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

    #[test]
    fn the_tools_act_only_on_the_apps_pages_or_a_blank_one() {
        let policy = local(3000);
        assert_eq!(policy.check_current(None), Ok("about:blank".to_owned()));
        assert_eq!(
            policy.check_current(Some("about:blank")),
            Ok("about:blank".to_owned())
        );
        assert_eq!(
            policy.check_current(Some("http://localhost:3000/login?next=/")),
            Ok("http://localhost:3000/login?next=/".to_owned())
        );
        for elsewhere in [
            "https://www.google.com/search?q=secret",
            "http://localhost:3001/",
            "data:text/html,hi",
            "file:///etc/hosts",
        ] {
            let refused = policy.check_current(Some(elsewhere));
            assert_eq!(refused, Err(BrowserError::OtherSite), "{elsewhere}");
            let message = refused.err().map(|e| e.to_string()).unwrap_or_default();
            assert!(!message.contains("google") && !message.contains("secret"));
            assert!(message.contains("page.open"), "{message}");
        }
    }

    #[test]
    fn the_user_may_browse_any_web_page_but_not_other_schemes() {
        for url in [
            "https://www.google.com/",
            "http://example.com:8080/x",
            "http://localhost:3001/",
            "about:blank",
            "data:text/html,hi",
            "blob:http://localhost:3000/1234",
        ] {
            assert_eq!(user_may_open(url), Ok(()), "{url}");
        }
        assert_eq!(
            user_may_open("file:///etc/passwd"),
            Err(BrowserError::UnsupportedScheme("file".into()))
        );
        assert!(user_may_open("javascript:alert(1)").is_err());
        assert!(user_may_open("zoommtg://join").is_err());
        assert!(matches!(
            user_may_open("not a url"),
            Err(BrowserError::InvalidUrl { .. })
        ));
    }

    #[test]
    fn typed_urls_get_a_scheme_or_the_apps_origin() {
        let base = "http://localhost:3000";
        assert_eq!(typed_url("", base), base);
        assert_eq!(typed_url("  ", base), base);
        assert_eq!(typed_url("/login", base), "http://localhost:3000/login");
        assert_eq!(
            typed_url("/a?b=1", "http://localhost:3000/app/"),
            "http://localhost:3000/a?b=1"
        );
        assert_eq!(typed_url("google.com", base), "https://google.com");
        assert_eq!(
            typed_url(" Example.com/path?q=1 ", base),
            "https://Example.com/path?q=1"
        );
        assert_eq!(
            typed_url("localhost:3000/x", base),
            "http://localhost:3000/x"
        );
        assert_eq!(typed_url("127.0.0.1:8080", base), "http://127.0.0.1:8080");
        assert_eq!(typed_url("[::1]:5173/", base), "http://[::1]:5173/");
        assert_eq!(typed_url("http://google.com", base), "http://google.com");
        assert_eq!(typed_url("HTTPS://x.dev/", base), "HTTPS://x.dev/");
        assert_eq!(typed_url("about:blank", base), "about:blank");
    }
}
