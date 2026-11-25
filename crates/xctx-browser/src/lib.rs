//! A minimal Chrome DevTools protocol client behind the browser plugin: find Chrome, start it with a
//! dedicated profile (or attach to one already running), open a page of the space's app, take a screenshot,
//! and collect its console and failed requests.
//!
//! Everything blocks (a plain socket for the `/json/*` endpoints and a `tungstenite` websocket for the
//! protocol, no async runtime); the app calls it from a background thread. Each [`Page`] has its own
//! connection and reads its events whenever it waits for an answer, keeping only bounded lists of what it
//! needs. Pages open only URLs their [`UrlPolicy`] allows (nothing by default; the app allows the space's
//! port on the machine itself and its base URLs), and [`Page::allowed_current_url`] checks where a page went
//! since. [`BrowserError::is_transport`] tells a broken connection (reset Chrome) from a failed page action.
//!
//! ```no_run
//! # fn main() -> xctx_browser::Result<()> {
//! use xctx_browser::{Browser, LaunchOptions, UrlPolicy, find_chrome};
//! let mut options = LaunchOptions::new(find_chrome()?, "/path/to/xctx/browser-profile");
//! options.headless = true;
//! let mut browser = Browser::launch(options)?;
//! let mut policy = UrlPolicy::default();
//! policy.allow_local_port(3000);
//! let mut page = browser.new_page(policy)?;
//! page.open("http://localhost:3000/")?;
//! page.allowed_current_url()?;
//! let png = page.screenshot()?;
//! let console = page.console()?;
//! let failed = page.network_errors()?;
//! # let _ = (png, console, failed);
//! browser.close()
//! # }
//! ```

#![warn(missing_docs)]

pub mod browser;
pub mod cdp;
pub mod chrome;
pub mod error;
mod http;
pub mod page;
pub mod policy;

pub use browser::{
    ACTIVE_PORT_FILE, ActivePort, Browser, DEFAULT_LAUNCH_TIMEOUT, LaunchOptions, TargetInfo,
    VersionInfo, parse_active_port,
};
pub use cdp::{Event, MAX_MESSAGE_BYTES};
pub use chrome::{CHROME_APPS, CHROME_ENV, find_chrome};
pub use error::{BrowserError, Result};
pub use page::{
    BoundedLog, Clip, ConsoleEntry, DEFAULT_PAGE_TIMEOUT, MAX_CONSOLE_ENTRIES, MAX_ENTRY_CHARS,
    MAX_NETWORK_ERRORS, NetworkError, Page, ScreenshotOptions,
};
pub use policy::{LOCAL_HOSTS, UrlPolicy};
