//! The browser plugin's page logic, without a window: what the live browser view (`xctx-web-view`, a
//! `WKWebView` in the space's browser pane) runs in the page and what it reads back.
//!
//! - [`UrlPolicy`]: which pages the agents' `page.*` tools may open and read (the space's own app). The user
//!   browses freely in the pane; the tools refuse a page outside the policy.
//! - [`script`]: the scripts the tools run in the page (click, type, read text), each built with its values
//!   encoded as JSON string literals so nothing from a selector or a text is ever spliced into code, and the
//!   parsing of what they return.
//! - [`log`]: the console messages and failed requests the capture script ([`log::capture_script`]) reports,
//!   parsed ([`log::parse_message`]) into bounded lists ([`PageLog`]).
//! - [`typed_url`]: what the pane's URL bar opens for what the user typed.
//!
//! Everything here is plain data and strings, tested without WebKit.

#![warn(missing_docs)]

pub mod error;
pub mod log;
pub mod policy;
pub mod script;

pub use error::{BrowserError, Result};
pub use log::{
    BoundedLog, ConsoleEntry, MAX_CONSOLE_ENTRIES, MAX_ENTRY_CHARS, MAX_NETWORK_ERRORS,
    MESSAGE_HANDLER, NetworkError, PageLog, PageMessage, capture_script, parse_message,
};
pub use policy::{LOCAL_HOSTS, UrlPolicy, typed_url, user_may_open};
pub use script::{
    CLICKED_TEXT_CHARS, Clicked, MAX_PAGE_TEXT_CHARS, PageText, Typed, cap_text, click_script,
    click_target, element_script, evaluation_script, js_string, page_text, parse_evaluation,
    read_value_script, text_script, type_script, typed_value,
};
