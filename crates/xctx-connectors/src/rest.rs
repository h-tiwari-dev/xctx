//! What the Jira and Figma connectors share: sending a JSON request to an allowed host through `xctx-http`,
//! turning error statuses into errors, basic auth, and Atlassian document format.

use serde_json::{Value, json};
use xctx_http::{Outcome, Request, SendOptions};

use crate::BuiltinContext;
use crate::error::{ConnectorError, Result};

/// The most bytes of an API response read.
pub(crate) const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// Options allowing `base_url`'s host and the context's extra hosts, and nothing else (not even localhost,
/// unless the base URL is local).
pub(crate) fn options(base_url: &str, ctx: &BuiltinContext) -> Result<SendOptions> {
    let host = xctx_http::host_of(base_url)
        .map_err(|e| ConnectorError::NotConfigured(format!("base URL {base_url}: {e}")))?;
    let mut options = SendOptions {
        timeout: ctx.timeout,
        max_body_bytes: MAX_RESPONSE_BYTES,
        allowed_hosts: Vec::new(),
        max_redirects: 0,
    };
    options.allow_host(&host);
    for h in &ctx.allowed_hosts {
        options.allow_host(h);
    }
    Ok(options)
}

/// A base URL setting without its trailing slashes, which must be `http` or `https`.
pub(crate) fn base_url(raw: &str) -> Result<String> {
    let base = raw.trim().trim_end_matches('/').to_owned();
    xctx_http::host_of(&base)
        .map_err(|e| ConnectorError::NotConfigured(format!("base URL {base}: {e}")))?;
    Ok(base)
}

/// Send `request` and return the parsed JSON body of a 2xx answer (`null` for an empty body). Other statuses
/// become [`ConnectorError::Service`] with the service's own message when it gives one.
pub(crate) fn send_json(service: &str, request: &Request, options: &SendOptions) -> Result<Value> {
    let exchange = xctx_http::send(request, options).map_err(|e| match e {
        xctx_http::HttpError::HostNotAllowed { .. } => ConnectorError::Refused(e.to_string()),
        other => ConnectorError::Http(other.to_string()),
    })?;
    let response = match exchange.outcome {
        Outcome::Response(r) => r,
        Outcome::Failed(f) => {
            return Err(ConnectorError::Http(format!("{service}: {}", f.message)));
        }
    };
    if !(200..300).contains(&response.status) {
        return Err(ConnectorError::Service {
            service: service.to_owned(),
            status: response.status,
            message: error_message(response.status, &response.body),
        });
    }
    if response.truncated {
        return Err(ConnectorError::Http(format!(
            "{service}'s answer was longer than {MAX_RESPONSE_BYTES} bytes"
        )));
    }
    if response.body.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&response.body).map_err(|e| {
        ConnectorError::Http(format!(
            "{service} answered something that is not JSON: {e}"
        ))
    })
}

/// A short message for an error status: the service's own error fields when present, else the start of the
/// body.
fn error_message(status: u16, body: &str) -> String {
    let hint = match status {
        401 => " (check the token)",
        403 => " (the token may not access this)",
        404 => " (not found, or not visible to this token)",
        429 => " (rate limited; try later)",
        _ => "",
    };
    let mut parts = Vec::new();
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        if let Some(list) = v.get("errorMessages").and_then(Value::as_array) {
            parts.extend(list.iter().filter_map(Value::as_str).map(str::to_owned));
        }
        if let Some(errors) = v.get("errors").and_then(Value::as_object) {
            parts.extend(
                errors
                    .iter()
                    .map(|(k, v)| format!("{k}: {}", v.as_str().unwrap_or(&v.to_string()))),
            );
        }
        for key in ["err", "message", "error"] {
            if let Some(s) = v.get(key).and_then(Value::as_str) {
                parts.push(s.to_owned());
            }
        }
    }
    if parts.is_empty() {
        let mut text: String = body.trim().chars().take(300).collect();
        if text.is_empty() {
            text = "no details".to_owned();
        }
        parts.push(text);
    }
    format!("{}{hint}", parts.join("; "))
}

/// `user:password` in standard base64, for a basic `Authorization` header.
pub(crate) fn basic_auth(user: &str, password: &str) -> String {
    base64(format!("{user}:{password}").as_bytes())
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ABC[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Plain text of an Atlassian document format (ADF) value, as Jira's v3 API returns descriptions and comments:
/// paragraphs and headings on their own lines, list items as `- ` or `1. `, code blocks kept, table cells joined
/// with ` | `, mentions, emoji, dates, cards and media as short text. A plain string is returned as is.
pub fn flatten_adf(doc: &Value) -> String {
    let mut out = String::new();
    block(doc, &mut out, 0);
    let lines: Vec<&str> = out.lines().map(str::trim_end).collect();
    // At most one blank line between blocks.
    let mut text = String::new();
    let mut blank = 0;
    for line in lines {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        text.push_str(line);
        text.push('\n');
    }
    text.trim().to_owned()
}

fn children(node: &Value) -> &[Value] {
    node.get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn attr<'a>(node: &'a Value, name: &str) -> Option<&'a str> {
    node.get("attrs")?.get(name)?.as_str()
}

fn block(node: &Value, out: &mut String, depth: usize) {
    if let Value::String(s) = node {
        out.push_str(s);
        return;
    }
    let kind = node.get("type").and_then(Value::as_str).unwrap_or("");
    let indent = "  ".repeat(depth);
    match kind {
        "doc" | "blockquote" | "panel" | "expand" | "nestedExpand" | "layoutSection"
        | "layoutColumn" | "bodiedExtension" => {
            for c in children(node) {
                block(c, out, depth);
            }
        }
        "paragraph" | "heading" => {
            out.push_str(&indent);
            inline_all(node, out);
            out.push_str("\n\n");
        }
        "bulletList" | "orderedList" => {
            let start = node
                .get("attrs")
                .and_then(|a| a.get("order"))
                .and_then(Value::as_u64)
                .unwrap_or(1);
            for (i, item) in children(node).iter().enumerate() {
                let marker = if kind == "orderedList" {
                    format!("{}. ", start + i as u64)
                } else {
                    "- ".to_owned()
                };
                let mut inner = String::new();
                for c in children(item) {
                    match c.get("type").and_then(Value::as_str) {
                        Some("bulletList" | "orderedList") => block(c, &mut inner, depth + 1),
                        _ => {
                            inline_all(c, &mut inner);
                            inner.push('\n');
                        }
                    }
                }
                out.push_str(&indent);
                out.push_str(&marker);
                out.push_str(inner.trim_start());
                if !inner.ends_with('\n') {
                    out.push('\n');
                }
            }
            out.push('\n');
        }
        "codeBlock" => {
            let lang = attr(node, "language").unwrap_or("");
            out.push_str(&format!("```{lang}\n"));
            inline_all(node, out);
            out.push_str("\n```\n\n");
        }
        "rule" => out.push_str("---\n\n"),
        "table" => {
            for row in children(node) {
                let cells: Vec<String> = children(row)
                    .iter()
                    .map(|cell| {
                        let mut s = String::new();
                        for c in children(cell) {
                            inline_all(c, &mut s);
                            s.push(' ');
                        }
                        s.trim().to_owned()
                    })
                    .collect();
                out.push_str(&cells.join(" | "));
                out.push('\n');
            }
            out.push('\n');
        }
        "mediaSingle" | "mediaGroup" => {
            for c in children(node) {
                inline(c, out);
            }
            out.push_str("\n\n");
        }
        _ => {
            inline(node, out);
        }
    }
}

fn inline_all(node: &Value, out: &mut String) {
    for c in children(node) {
        inline(c, out);
    }
}

fn inline(node: &Value, out: &mut String) {
    let kind = node.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "text" => out.push_str(node.get("text").and_then(Value::as_str).unwrap_or("")),
        "hardBreak" => out.push('\n'),
        "mention" => out.push_str(attr(node, "text").unwrap_or("@someone")),
        "emoji" => out.push_str(
            attr(node, "text")
                .or_else(|| attr(node, "shortName"))
                .unwrap_or(""),
        ),
        "inlineCard" | "blockCard" | "embedCard" => {
            out.push_str(attr(node, "url").unwrap_or("[link]"))
        }
        "date" => out.push_str(attr(node, "timestamp").unwrap_or("[date]")),
        "status" => {
            out.push('[');
            out.push_str(attr(node, "text").unwrap_or(""));
            out.push(']');
        }
        "media" | "mediaInline" => {
            out.push_str("[media");
            if let Some(alt) = attr(node, "alt") {
                out.push_str(": ");
                out.push_str(alt);
            }
            out.push(']');
        }
        _ => {
            // Unknown inline or block: keep whatever text it holds.
            let mut inner = String::new();
            for c in children(node) {
                block(c, &mut inner, 0);
            }
            out.push_str(inner.trim_end());
        }
    }
}

/// An ADF document for plain text: paragraphs at blank lines, hard breaks at single newlines.
pub(crate) fn text_to_adf(text: &str) -> Value {
    let normalized = text.replace("\r\n", "\n");
    let paragraphs: Vec<Value> = normalized
        .split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            let mut content = Vec::new();
            for (i, line) in p.trim_matches('\n').split('\n').enumerate() {
                if i > 0 {
                    content.push(json!({"type": "hardBreak"}));
                }
                if !line.is_empty() {
                    content.push(json!({"type": "text", "text": line}));
                }
            }
            json!({"type": "paragraph", "content": content})
        })
        .collect();
    json!({"type": "doc", "version": 1, "content": paragraphs})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(basic_auth("me@x.io", "tok"), "bWVAeC5pbzp0b2s=");
    }

    #[test]
    fn adf_flattens_to_readable_text() {
        let doc = json!({"type": "doc", "version": 1, "content": [
            {"type": "heading", "attrs": {"level": 2}, "content": [{"type": "text", "text": "Steps"}]},
            {"type": "paragraph", "content": [
                {"type": "text", "text": "Hi "},
                {"type": "mention", "attrs": {"id": "1", "text": "@Ana"}},
                {"type": "text", "text": ", see"},
                {"type": "hardBreak"},
                {"type": "inlineCard", "attrs": {"url": "https://x.io/a"}},
                {"type": "emoji", "attrs": {"shortName": ":smile:", "text": "😄"}}
            ]},
            {"type": "bulletList", "content": [
                {"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "one"}]}]},
                {"type": "listItem", "content": [
                    {"type": "paragraph", "content": [{"type": "text", "text": "two"}]},
                    {"type": "orderedList", "content": [
                        {"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "nested"}]}]}
                    ]}
                ]}
            ]},
            {"type": "codeBlock", "attrs": {"language": "sh"}, "content": [{"type": "text", "text": "make test"}]},
            {"type": "table", "content": [
                {"type": "tableRow", "content": [
                    {"type": "tableHeader", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "a"}]}]},
                    {"type": "tableCell", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "b"}]}]}
                ]}
            ]},
            {"type": "mediaSingle", "content": [{"type": "media", "attrs": {"alt": "shot.png"}}]},
            {"type": "unknownThing", "content": [{"type": "text", "text": "kept"}]}
        ]});
        let text = flatten_adf(&doc);
        assert_eq!(
            text,
            "Steps\n\nHi @Ana, see\nhttps://x.io/a😄\n\n- one\n- two\n  1. nested\n\n```sh\nmake test\n```\n\n\
             a | b\n\n[media: shot.png]\n\nkept"
        );
        assert_eq!(flatten_adf(&json!("plain")), "plain");
        assert_eq!(flatten_adf(&Value::Null), "");
    }

    #[test]
    fn text_becomes_adf_paragraphs() {
        let doc = text_to_adf("first line\nsecond\n\nnew paragraph");
        assert_eq!(
            doc,
            json!({"type": "doc", "version": 1, "content": [
                {"type": "paragraph", "content": [
                    {"type": "text", "text": "first line"}, {"type": "hardBreak"}, {"type": "text", "text": "second"}
                ]},
                {"type": "paragraph", "content": [{"type": "text", "text": "new paragraph"}]}
            ]})
        );
        assert_eq!(flatten_adf(&doc), "first line\nsecond\n\nnew paragraph");
    }

    #[test]
    fn error_messages_prefer_the_services_own() {
        assert_eq!(
            error_message(
                404,
                r#"{"errorMessages":["Issue does not exist"],"errors":{}}"#
            ),
            "Issue does not exist (not found, or not visible to this token)"
        );
        assert_eq!(
            error_message(403, r#"{"status":403,"err":"Invalid token"}"#),
            "Invalid token (the token may not access this)"
        );
        assert_eq!(error_message(500, ""), "no details");
    }
}
