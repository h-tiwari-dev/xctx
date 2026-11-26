//! Hint bubbles for local servers. Every few seconds the terminals of open spaces whose screen changed are read
//! for a local URL (`http://localhost:3000`, `127.0.0.1:8000/api`, "listening on port 4000"). When something
//! answers on its port, a bubble on that pane offers to open it beside the agent: the space's browser for a site,
//! the API client for an API. When an agent pane names a local URL nobody serves yet and its repo has a dev
//! script, the bubble offers to start it in a terminal pane. Each URL is offered once per space and state.

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use gpui::{AnyElement, Context, FontWeight, SharedString, Task, Window, div, prelude::*, px};
use xctx_core::SpaceId;
use xctx_layout::{PaneId, PaneKind};
use xctx_ui::{Button, ButtonSize, ButtonVariant, Theme};

use crate::plugin_host::MountSettings;
use crate::space_tab::PaneTerminal;
use crate::workspace::Workspace;

/// How often the open spaces' terminals are read for URLs.
const SCAN_INTERVAL: Duration = Duration::from_millis(1500);

/// How long a port gets to accept a connection; loopback answers at once either way.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(150);

/// Hosts that mean "this machine".
const LOCAL_HOSTS: &[&str] = &["localhost", "127.0.0.1", "0.0.0.0", "[::1]", "[::]"];

/// Words on a URL's line, or the start of its path, that say it is an API rather than a site.
const API_WORDS: &[&str] = &[
    "api", "graphql", "swagger", "openapi", "uvicorn", "fastapi", "grpc", "gunicorn",
];

/// What was served.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ServedKind {
    /// A web page: opened in the space's browser.
    Site,
    /// An API: opened in the API client.
    Api,
}

/// A local URL found in a terminal's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocalUrl {
    /// As opened: `http(s)://localhost:<port><path>`.
    pub(crate) url: String,
    /// Its port.
    pub(crate) port: u16,
    /// Whether it looks like a site or an API.
    pub(crate) kind: ServedKind,
}

/// What a bubble offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HintAction {
    /// Open the running server beside the agent.
    Open(ServedKind),
    /// Start the repo's dev server (this argv) in a terminal pane.
    Start(Vec<String>),
}

/// The bubble on one pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ServeHint {
    /// The URL it is about.
    pub(crate) url: LocalUrl,
    /// What its button does.
    pub(crate) action: HintAction,
}

/// What the hint scanner remembers.
#[derive(Default)]
pub(crate) struct ServeHints {
    /// The screen generation last read, by space and pane.
    scanned: HashMap<(SpaceId, PaneId), u64>,
    /// The URLs offered already, by space, with whether they were running then.
    offered: HashSet<(SpaceId, String, bool)>,
    /// The bubble shown on each pane.
    shown: HashMap<(SpaceId, PaneId), ServeHint>,
    _task: Option<Task<()>>,
}

/// The last local URL in `text`, the most recent line on a terminal screen.
pub(crate) fn last_local_url(text: &str) -> Option<LocalUrl> {
    text.lines().rev().find_map(|line| {
        let mut found = line
            .split(|c: char| {
                c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>' | '(' | ')')
            })
            .filter_map(url_in_token)
            .next_back()
            .or_else(|| port_phrase(line))?;
        found.kind = kind_of(&found.url, line);
        Some(found)
    })
}

/// Whether `open` (a browser's URL) is the page `url` names, ignoring a trailing `/` and `127.0.0.1` for
/// `localhost`.
fn same_page(open: &str, url: &str) -> bool {
    let norm = |u: &str| {
        u.replace("://127.0.0.1:", "://localhost:")
            .trim_end_matches('/')
            .to_owned()
    };
    norm(open) == norm(url)
}

/// `token` as a local URL: `http(s)://host:port[/path]` or a bare `host:port[/path]` with a local host.
fn url_in_token(token: &str) -> Option<LocalUrl> {
    let token = token.trim_end_matches(['.', ',', ';', ':', '!', '?']);
    let (https, rest) = match token.to_ascii_lowercase() {
        t if t.starts_with("https://") => (true, &token["https://".len()..]),
        t if t.starts_with("http://") => (false, &token["http://".len()..]),
        _ => (false, token),
    };
    let lower = rest.to_ascii_lowercase();
    let host = LOCAL_HOSTS
        .iter()
        .find(|h| lower.starts_with(&format!("{h}:")))?;
    let after = &rest[host.len() + 1..];
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    let port: u16 = digits.parse().ok().filter(|p| *p > 0)?;
    let path = &after[digits.len()..];
    if !path.is_empty() && !path.starts_with('/') {
        return None;
    }
    let scheme = if https { "https" } else { "http" };
    let path = if path.is_empty() { "/" } else { path };
    Some(LocalUrl {
        url: format!("{scheme}://localhost:{port}{path}"),
        port,
        kind: ServedKind::Site,
    })
}

/// A "listening on port 4000" style line, as `http://localhost:4000/`.
fn port_phrase(line: &str) -> Option<LocalUrl> {
    let lower = line.to_ascii_lowercase();
    if !["listen", "running", "started", "serving", "ready"]
        .iter()
        .any(|w| lower.contains(w))
    {
        return None;
    }
    let words: Vec<&str> = lower.split_whitespace().collect();
    words.windows(2).find_map(|pair| {
        let port: u16 = (pair[0] == "port")
            .then(|| pair[1].trim_end_matches(['.', ',', '!']).parse().ok())
            .flatten()
            .filter(|p| *p > 0)?;
        Some(LocalUrl {
            url: format!("http://localhost:{port}/"),
            port,
            kind: ServedKind::Site,
        })
    })
}

fn kind_of(url: &str, line: &str) -> ServedKind {
    let path = url
        .splitn(4, '/')
        .nth(3)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let line = line.to_ascii_lowercase();
    let api_path = ["api", "graphql", "docs", "swagger", "v1"]
        .iter()
        .any(|p| path.starts_with(p));
    let api_word = line
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| API_WORDS.contains(&w));
    if api_path || api_word {
        ServedKind::Api
    } else {
        ServedKind::Site
    }
}

/// Whether something accepts connections on `port` of this machine.
pub(crate) fn is_listening(port: u16) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).is_ok()
}

impl Workspace {
    /// Start reading the open spaces' terminals for local URLs, every [`SCAN_INTERVAL`].
    pub(crate) fn start_serve_hints(&mut self, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        self.serve_hints._task = Some(cx.spawn(async move |this, cx| {
            loop {
                executor.timer(SCAN_INTERVAL).await;
                if this
                    .update(cx, |this, cx| this.scan_serve_hints(cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    /// Read every running terminal of the open spaces whose screen changed, and show a bubble for a new URL.
    fn scan_serve_hints(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        let mut found: Vec<(SpaceId, PaneId, ServeHint, bool)> = Vec::new();
        for (id, tab) in &self.tabs {
            for pane in tab.layout.panes() {
                let Some(PaneTerminal::Running { view, .. }) = tab.terminals.get(&pane.id) else {
                    continue;
                };
                let session = view.read(cx).session();
                let generation = session.generation();
                let key = (id.clone(), pane.id);
                if self.serve_hints.scanned.get(&key) == Some(&generation) {
                    continue;
                }
                self.serve_hints.scanned.insert(key, generation);
                let Some(url) = last_local_url(&session.snapshot().text()) else {
                    continue;
                };
                let running = is_listening(url.port);
                // The space's browser already shows this page: nothing to offer.
                let shown = self
                    .browsers
                    .view(id)
                    .and_then(|view| view.url())
                    .is_some_and(|open| same_page(&open, &url.url));
                if running && url.kind == ServedKind::Site && shown {
                    continue;
                }
                if self
                    .serve_hints
                    .offered
                    .contains(&(id.clone(), url.url.clone(), running))
                {
                    continue;
                }
                // Offering to start a server is only for an agent that names one nobody serves yet.
                let action = if running {
                    HintAction::Open(url.kind)
                } else {
                    let start = (pane.role.kind == PaneKind::Agent)
                        .then(|| crate::spawn::pane_worktree(&tab.space, &pane.role))
                        .flatten()
                        .and_then(crate::dev_setup::web_app);
                    match start {
                        Some(app) => HintAction::Start(app.command),
                        None => continue,
                    }
                };
                found.push((id.clone(), pane.id, ServeHint { url, action }, running));
            }
        }
        for (id, pane, hint, running) in found {
            self.serve_hints
                .offered
                .insert((id.clone(), hint.url.url.clone(), running));
            self.serve_hints.shown.insert((id, pane), hint);
            changed = true;
        }
        // Forget panes and spaces that are gone, and close a site's bubble once the browser shows that page.
        let (tabs, browsers) = (&self.tabs, &self.browsers);
        self.serve_hints
            .scanned
            .retain(|(id, pane), _| tabs.get(id).is_some_and(|t| t.terminals.contains_key(pane)));
        let before = self.serve_hints.shown.len();
        self.serve_hints.shown.retain(|(id, pane), hint| {
            let open = hint.action == HintAction::Open(ServedKind::Site)
                && browsers
                    .view(id)
                    .and_then(|view| view.url())
                    .is_some_and(|open| same_page(&open, &hint.url.url));
            !open && tabs.get(id).is_some_and(|t| t.terminals.contains_key(pane))
        });
        if changed || before != self.serve_hints.shown.len() {
            cx.notify();
        }
    }

    /// Close the bubble on `pane` of space `id`.
    fn dismiss_serve_hint(&mut self, id: &SpaceId, pane: PaneId, cx: &mut Context<Self>) {
        if self.serve_hints.shown.remove(&(id.clone(), pane)).is_some() {
            cx.notify();
        }
    }

    /// Do what the bubble on `pane` of space `id` offers, then close it.
    fn act_on_serve_hint(
        &mut self,
        id: &SpaceId,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(hint) = self.serve_hints.shown.remove(&(id.clone(), pane)) else {
            return;
        };
        let repo = self
            .tabs
            .get(id)
            .and_then(|tab| tab.layout.panes().into_iter().find(|p| p.id == pane))
            .map(|p| p.role.repo.clone());
        let result = match hint.action {
            HintAction::Open(kind) => self.open_served(id, repo, &hint.url.url, kind, window, cx),
            HintAction::Start(command) => self
                .mount_plugin(
                    id,
                    "terminal",
                    repo,
                    MountSettings {
                        command: Some(command),
                        ..MountSettings::default()
                    },
                    false,
                    window,
                    cx,
                )
                .map(|_| ()),
        };
        if let Err(e) = result {
            self.show_op_error(e, cx);
        }
        cx.notify();
    }

    /// Show `url` beside the agents: in the space's browser for a site (mounting one when there is none), in the
    /// API client for an API.
    fn open_served(
        &mut self,
        id: &SpaceId,
        repo: Option<String>,
        url: &str,
        kind: ServedKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), xctx_control::RpcError> {
        let (plugin, plugin_kind, settings) = match kind {
            ServedKind::Site => (
                "browser",
                "web",
                MountSettings {
                    url: Some(url.to_owned()),
                    ..MountSettings::default()
                },
            ),
            ServedKind::Api => (
                "api-client",
                "http",
                MountSettings {
                    base_url: Some(url.trim_end_matches('/').to_owned()),
                    ..MountSettings::default()
                },
            ),
        };
        let mounted = self
            .mounted_plugins(id)
            .into_iter()
            .find(|m| m.kind == plugin_kind);
        let Some(mounted) = mounted else {
            return self
                .mount_plugin(id, plugin, repo, settings, false, window, cx)
                .map(|_| ());
        };
        if kind == ServedKind::Site {
            let view = self
                .space_browser(id, None, cx)
                .map_err(xctx_control::RpcError::conflict)?;
            view.load(url)
                .map_err(|e| xctx_control::RpcError::conflict(e.to_string()))?;
        }
        self.focus_pane_in(id, mounted.pane, window, cx)
    }

    /// The bubble on `pane` of space `id`, drawn over the pane's top right corner, if it has one.
    pub(crate) fn render_serve_hint(
        &self,
        id: &SpaceId,
        pane: PaneId,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let hint = self.serve_hints.shown.get(&(id.clone(), pane))?;
        let theme = Theme::of(cx);
        let address = hint
            .url
            .url
            .split_once("://")
            .map(|(_, rest)| rest.trim_end_matches('/'))
            .unwrap_or(&hint.url.url)
            .to_owned();
        let (headline, button) = match &hint.action {
            HintAction::Open(ServedKind::Site) => {
                (format!("{address} is running"), "Open beside".to_owned())
            }
            HintAction::Open(ServedKind::Api) => {
                (format!("API on {address}"), "Open API client".to_owned())
            }
            HintAction::Start(command) => (
                format!("Nothing is serving {address} yet"),
                format!("Run {}", command.join(" ")),
            ),
        };
        let (act, dismiss) = (id.clone(), id.clone());
        let selector = format!("serve-hint-{}", pane.0);
        Some(
            div()
                .debug_selector(move || selector)
                .absolute()
                .top(Theme::SPACE_2)
                .right(Theme::SPACE_3)
                .max_w(px(360.0))
                .flex()
                .items_center()
                .gap(Theme::SPACE_2)
                .pl(Theme::SPACE_3)
                .pr(Theme::SPACE_1)
                .py(Theme::SPACE_1)
                .rounded(Theme::RADIUS_LG)
                .border_1()
                .border_color(theme.accent)
                .bg(theme.raised)
                .shadow_lg()
                .child(
                    div()
                        .flex_none()
                        .size(px(8.0))
                        .rounded(Theme::RADIUS_FULL)
                        .bg(if matches!(hint.action, HintAction::Start(_)) {
                            theme.warning
                        } else {
                            theme.success
                        }),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(Theme::TEXT_SM)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(headline),
                )
                .child(
                    Button::new(
                        SharedString::from(format!("serve-hint-act-{}", pane.0)),
                        button,
                    )
                    .variant(ButtonVariant::Primary)
                    .size(ButtonSize::Compact)
                    .selector("serve-hint-act")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.act_on_serve_hint(&act, pane, window, cx);
                    })),
                )
                .child(
                    Button::new(
                        SharedString::from(format!("serve-hint-dismiss-{}", pane.0)),
                        "×",
                    )
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .selector("serve-hint-dismiss")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.dismiss_serve_hint(&dismiss, pane, cx);
                    })),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Option<(String, ServedKind)> {
        last_local_url(text).map(|u| (u.url, u.kind))
    }

    #[test]
    fn dev_servers_print_their_local_url() {
        let next = "  ▲ Next.js 15.0.0\n  - Local:        http://localhost:4001\n  - Network:      http://192.168.1.4:4001\n\n ✓ Ready in 1.2s";
        assert_eq!(
            url(next),
            Some(("http://localhost:4001/".into(), ServedKind::Site))
        );
        let vite = "  VITE v5.0.0  ready in 300 ms\n\n  ➜  Local:   http://127.0.0.1:5173/\n  ➜  press h + enter to show help";
        assert_eq!(
            url(vite),
            Some(("http://localhost:5173/".into(), ServedKind::Site))
        );
        assert_eq!(
            url("Server listening on port 8080."),
            Some(("http://localhost:8080/".into(), ServedKind::Site))
        );
        assert_eq!(
            url("(see http://0.0.0.0:3000/dashboard),"),
            Some(("http://localhost:3000/dashboard".into(), ServedKind::Site))
        );
    }

    #[test]
    fn apis_are_told_apart_from_sites() {
        assert_eq!(
            url("INFO:     Uvicorn running on http://127.0.0.1:8000 (Press CTRL+C to quit)"),
            Some(("http://localhost:8000/".into(), ServedKind::Api))
        );
        assert_eq!(
            url("GraphQL ready at localhost:4000/graphql"),
            Some(("http://localhost:4000/graphql".into(), ServedKind::Api))
        );
        assert_eq!(
            url("the api is up on http://localhost:3001"),
            Some(("http://localhost:3001/".into(), ServedKind::Api))
        );
    }

    #[test]
    fn the_newest_line_wins_and_other_hosts_are_ignored() {
        let text = "Local: http://localhost:3000\nrestarted\nLocal: http://localhost:3001\n";
        assert_eq!(
            url(text).map(|(u, _)| u),
            Some("http://localhost:3001/".into())
        );
        assert_eq!(url("Network: http://192.168.1.4:4001"), None);
        assert_eq!(url("see https://example.com:443/docs"), None);
        assert_eq!(url("localhost:abc and localhost:0"), None);
        assert_eq!(
            url("the port 3000 in config"),
            None,
            "a port alone needs a listening phrase"
        );
    }

    #[test]
    fn a_port_with_a_listener_is_listening() -> Result<(), Box<dyn std::error::Error>> {
        // Only the open case: after the listener drops, another test may take the freed port at once.
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        assert!(is_listening(listener.local_addr()?.port()));
        Ok(())
    }
}
