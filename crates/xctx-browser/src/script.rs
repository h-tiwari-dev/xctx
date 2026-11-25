//! The scripts the `page.*` tools run in the page, and what they return.
//!
//! Every value a script needs (a selector, a text) goes in as a JSON string literal ([`js_string`]), with `<`
//! escaped as well, so a quote, a backslash, a backtick or `</script>` in it is data, never code. Scripts are
//! single expressions; [`evaluation_script`] wraps one so it always answers with a JSON string
//! (`{"ok": value}` or `{"error": text}`), which [`parse_evaluation`] reads.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{BrowserError, Result};

/// The most characters [`page_text`] keeps of the page's text.
pub const MAX_PAGE_TEXT_CHARS: usize = 20_000;

/// How many characters of a clicked element's text [`Clicked::text`] keeps.
pub const CLICKED_TEXT_CHARS: usize = 80;

/// What `page.click` clicked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clicked {
    /// The element's tag name, lowercase (`button`, `a`).
    pub tag: String,
    /// The element's visible text (or value), whitespace collapsed, cut to [`CLICKED_TEXT_CHARS`].
    pub text: String,
    /// The page's URL after the click (and any navigation it caused).
    pub url: String,
}

/// What `page.type` left in the field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Typed {
    /// The field's value afterwards, or `None` when it is gone (a submit navigated away).
    pub value: Option<String>,
    /// The page's URL afterwards.
    pub url: String,
}

/// What `page.text` read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageText {
    /// The visible text, at most [`MAX_PAGE_TEXT_CHARS`] characters.
    pub text: String,
    /// Whether the text was cut.
    pub truncated: bool,
    /// How many characters the whole text had.
    pub chars: usize,
    /// The page's URL.
    pub url: String,
}

/// `value` as a JavaScript string literal: JSON-encoded, with `<` escaped too so `</script>` cannot close
/// anything should the script ever be embedded in HTML, and the line and paragraph separators escaped for
/// engines that read them as line breaks.
pub fn js_string(value: &str) -> String {
    serde_json::to_string(value)
        .unwrap_or_else(|_| "\"\"".to_owned())
        .replace('<', "\\u003c")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

/// A script that runs `body` with `el` bound to the first element matching `selector` (or to
/// `document.body` when `selector` is `None`), answering `null` when nothing matches.
pub fn element_script(selector: Option<&str>, body: &str) -> String {
    let find = match selector {
        Some(selector) => format!("document.querySelector({})", js_string(selector)),
        None => "document.body".to_owned(),
    };
    format!("(() => {{ const el = {find}; if (!el) return null; {body} }})()")
}

/// `script` (an expression) wrapped so it answers with a JSON string: `{"ok": value}` (`undefined` as
/// `null`), or `{"error": text}` when it throws.
pub fn evaluation_script(script: &str) -> String {
    format!(
        "(() => {{ try {{ const value = ({script}); \
         return JSON.stringify({{ ok: value === undefined ? null : value }}); }} \
         catch (e) {{ return JSON.stringify({{ error: String(e && e.message ? e.message : e) }}); }} }})()"
    )
}

/// The value an [`evaluation_script`] answered with `answer`, or the script's error.
pub fn parse_evaluation(answer: &str) -> Result<Value> {
    let mut parsed: Value = serde_json::from_str(answer)
        .map_err(|e| BrowserError::Script(format!("unreadable answer: {e}")))?;
    if let Some(error) = parsed.get("error") {
        let text = error
            .as_str()
            .map_or_else(|| error.to_string(), str::to_owned);
        return Err(BrowserError::Script(text));
    }
    match parsed.get_mut("ok") {
        Some(value) => Ok(value.take()),
        None => Err(BrowserError::Script("the script gave no answer".to_owned())),
    }
}

const CLICK: &str = "el.scrollIntoView({ block: 'center', inline: 'center' }); \
    const r = el.getBoundingClientRect(); \
    const text = (el.innerText || el.value || '').replace(/\\s+/g, ' ').trim(); \
    const boxed = r.width > 0 && r.height > 0; \
    if (boxed) { \
      const x = r.left + r.width / 2, y = r.top + r.height / 2; \
      const mouse = { bubbles: true, cancelable: true, composed: true, view: window, clientX: x, clientY: y, button: 0 }; \
      const pointer = Object.assign({ pointerId: 1, pointerType: 'mouse', isPrimary: true }, mouse); \
      const Pointer = window.PointerEvent || MouseEvent; \
      el.dispatchEvent(new Pointer('pointerover', pointer)); \
      el.dispatchEvent(new MouseEvent('mouseover', mouse)); \
      el.dispatchEvent(new Pointer('pointerdown', Object.assign({ buttons: 1 }, pointer))); \
      el.dispatchEvent(new MouseEvent('mousedown', Object.assign({ buttons: 1 }, mouse))); \
      if (typeof el.focus === 'function') el.focus({ preventScroll: true }); \
      el.dispatchEvent(new Pointer('pointerup', pointer)); \
      el.dispatchEvent(new MouseEvent('mouseup', mouse)); \
      el.click(); \
    } \
    return { tag: el.tagName.toLowerCase(), text, boxed };";

/// The script of `page.click`: scrolls the first element matching `selector` into view, sends it pointer and
/// mouse events at its centre, then `click()`s it. Answers `{tag, text, boxed}` (`boxed` false when the
/// element has no box, in which case nothing was clicked), or `null` when nothing matches. Read it with
/// [`click_target`].
pub fn click_script(selector: &str) -> String {
    element_script(Some(selector), CLICK)
}

/// The tag and text (cut to [`CLICKED_TEXT_CHARS`]) of the element a [`click_script`] clicked, from its
/// `answer`, or why nothing was clicked.
pub fn click_target(selector: &str, answer: &Value) -> Result<(String, String)> {
    if answer.is_null() {
        return Err(BrowserError::element(selector, "no element matches"));
    }
    if answer.get("boxed").and_then(Value::as_bool) != Some(true) {
        return Err(BrowserError::element(
            selector,
            "the element has no box to click (hidden or empty)",
        ));
    }
    let text = answer.get("text").and_then(Value::as_str).unwrap_or("");
    let tag = answer.get("tag").and_then(Value::as_str).unwrap_or("");
    Ok((tag.to_owned(), cap_text(text, CLICKED_TEXT_CHARS).0))
}

const TYPE: &str = "const text = __TEXT__; \
    if (typeof el.focus === 'function') el.focus(); \
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype \
      : el instanceof HTMLInputElement ? HTMLInputElement.prototype : null; \
    if (proto) { \
      const set = Object.getOwnPropertyDescriptor(proto, 'value').set; \
      set.call(el, ''); \
      el.dispatchEvent(new Event('input', { bubbles: true })); \
      set.call(el, text); \
    } else if (el.isContentEditable) { \
      el.textContent = text; \
    } else { \
      return { editable: false }; \
    } \
    el.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: text })); \
    el.dispatchEvent(new Event('change', { bubbles: true })); \
    const value = 'value' in el && typeof el.value === 'string' ? el.value : el.innerText; \
    if (__SUBMIT__) { \
      const key = (type) => el.dispatchEvent(new KeyboardEvent(type, { key: 'Enter', code: 'Enter', \
        keyCode: 13, which: 13, bubbles: true, cancelable: true })); \
      const proceed = key('keydown'); \
      key('keypress'); \
      key('keyup'); \
      if (proceed && el instanceof HTMLInputElement && el.form) { \
        if (typeof el.form.requestSubmit === 'function') el.form.requestSubmit(); else el.form.submit(); \
      } \
    } \
    return { editable: true, value };";

/// The script of `page.type`: focuses the first element matching `selector` (an input, a textarea or an
/// editable element), clears it and sets `text` through the native value setter (so frameworks that track the
/// value see the change), firing `input` and `change`. With `submit`, it then sends Enter (`keydown`,
/// `keypress`, `keyup`) and, unless the page cancelled the key, submits the input's form with
/// `requestSubmit()`. Answers `{editable, value}`, or `null` when nothing matches. Read it with
/// [`typed_value`].
pub fn type_script(selector: &str, text: &str, submit: bool) -> String {
    let body = TYPE
        .replace("__TEXT__", &js_string(text))
        .replace("__SUBMIT__", if submit { "true" } else { "false" });
    element_script(Some(selector), &body)
}

/// The value a [`type_script`] left in the field, from its `answer`, or why it could not type.
pub fn typed_value(selector: &str, answer: &Value) -> Result<Option<String>> {
    if answer.is_null() {
        return Err(BrowserError::element(selector, "no element matches"));
    }
    if answer.get("editable").and_then(Value::as_bool) != Some(true) {
        return Err(BrowserError::element(
            selector,
            "the element cannot take text",
        ));
    }
    Ok(answer
        .get("value")
        .and_then(Value::as_str)
        .map(str::to_owned))
}

/// A script answering the value (or the text, for an editable element) of the first element matching
/// `selector`, or `null` when nothing matches (a submit navigated away).
pub fn read_value_script(selector: &str) -> String {
    element_script(
        Some(selector),
        "return 'value' in el && typeof el.value === 'string' ? el.value : el.innerText;",
    )
}

/// The script of `page.text`: the `innerText` of the first element matching `selector`, or of the body.
/// Answers a string, or `null` when nothing matches. Read it with [`page_text`].
pub fn text_script(selector: Option<&str>) -> String {
    element_script(selector, "return el.innerText;")
}

/// What `page.text` returns for the `answer` of a [`text_script`] on the page at `url`: the text cut to
/// [`MAX_PAGE_TEXT_CHARS`], or an error when nothing matched.
pub fn page_text(selector: Option<&str>, answer: &Value, url: String) -> Result<PageText> {
    let Some(full) = answer.as_str() else {
        return Err(BrowserError::element(
            selector.unwrap_or("body"),
            "no element matches",
        ));
    };
    let (text, truncated) = cap_text(full, MAX_PAGE_TEXT_CHARS);
    Ok(PageText {
        text,
        truncated,
        chars: full.chars().count(),
        url,
    })
}

/// `text` cut to `max` characters, and whether it was cut.
pub fn cap_text(text: &str, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((at, _)) => (text[..at].to_owned(), true),
        None => (text.to_owned(), false),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const NASTY: &str = r#"a"b'c\d</script><script>alert(1)</script>`${x}`"#;

    fn literal_reads_back(literal: &str) -> String {
        serde_json::from_str(literal).unwrap_or_default()
    }

    #[test]
    fn values_are_json_string_literals_without_a_raw_lt() {
        let literal = js_string(NASTY);
        assert!(!literal.contains('<'), "{literal}");
        assert!(!literal.contains("</script>"), "{literal}");
        assert!(literal.contains(r#"\""#) && literal.contains(r"\\"));
        assert!(
            literal.contains('`'),
            "a backtick is plain data in a JSON literal"
        );
        assert_eq!(literal_reads_back(&literal), NASTY);
        assert_eq!(js_string("line\nbreak\u{2028}"), r#""line\nbreak\u2028""#);
    }

    #[test]
    fn selectors_and_texts_are_never_spliced_raw() {
        let text = "hi `${alert(1)}` \\ \" </script>";
        for script in [
            click_script(NASTY),
            type_script(NASTY, text, true),
            read_value_script(NASTY),
            text_script(Some(NASTY)),
        ] {
            assert!(
                script.contains(&format!("document.querySelector({})", js_string(NASTY))),
                "{script}"
            );
            assert!(!script.contains(NASTY), "{script}");
            assert!(!script.contains("</script>"), "{script}");
        }
        let typed = type_script("#q", text, false);
        assert!(typed.contains(&format!("const text = {};", js_string(text))));
        assert!(typed.contains("if (false)"));
        assert!(type_script("#q", "", true).contains("if (true)"));
        assert!(!typed.contains("__TEXT__") && !typed.contains("__SUBMIT__"));
        assert!(text_script(None).contains("const el = document.body;"));
    }

    #[test]
    fn the_click_script_scrolls_dispatches_and_clicks() {
        let script = click_script("#go");
        let scroll = script.find("scrollIntoView");
        let down = script.find("'pointerdown'");
        let click = script.find("el.click()");
        assert!(scroll < down && down < click, "{script}");
        assert!(script.contains("'mousedown'") && script.contains("'mouseup'"));
    }

    #[test]
    fn evaluations_answer_json_and_errors() {
        let script = evaluation_script("1 + 1");
        assert!(script.contains("const value = (1 + 1);"), "{script}");
        assert_eq!(
            parse_evaluation(r#"{"ok":{"a":[1]}}"#),
            Ok(json!({"a": [1]}))
        );
        assert_eq!(parse_evaluation(r#"{"ok":null}"#), Ok(Value::Null));
        assert_eq!(
            parse_evaluation(r#"{"error":"x is not defined"}"#),
            Err(BrowserError::Script("x is not defined".into()))
        );
        assert!(parse_evaluation("{}").is_err());
        assert!(parse_evaluation("nope").is_err());
    }

    #[test]
    fn click_answers_become_targets_or_errors() {
        assert_eq!(
            click_target(
                "#go",
                &json!({"tag": "button", "text": "Go now", "boxed": true})
            ),
            Ok(("button".to_owned(), "Go now".to_owned()))
        );
        let long = "x".repeat(200);
        assert_eq!(
            click_target("a", &json!({"tag": "a", "text": long, "boxed": true}))
                .map(|(_, text)| text.chars().count()),
            Ok(CLICKED_TEXT_CHARS)
        );
        assert!(matches!(
            click_target("#none", &Value::Null),
            Err(BrowserError::Element { reason, .. }) if reason == "no element matches"
        ));
        assert!(matches!(
            click_target("#hidden", &json!({"tag": "button", "boxed": false})),
            Err(BrowserError::Element { reason, .. }) if reason.contains("no box")
        ));
    }

    #[test]
    fn type_answers_become_values_or_errors() {
        assert_eq!(
            typed_value("#q", &json!({"editable": true, "value": "hi"})),
            Ok(Some("hi".to_owned()))
        );
        assert!(typed_value("#none", &Value::Null).is_err());
        assert!(matches!(
            typed_value("div", &json!({"editable": false})),
            Err(BrowserError::Element { reason, .. }) if reason.contains("cannot take text")
        ));
    }

    #[test]
    fn page_text_is_capped_on_a_character_boundary() {
        assert_eq!(cap_text("héllo", 10), ("héllo".to_owned(), false));
        assert_eq!(cap_text("héllo", 5), ("héllo".to_owned(), false));
        assert_eq!(cap_text("héllo", 2), ("hé".to_owned(), true));
        assert_eq!(cap_text("", 0), (String::new(), false));
        let long = "é".repeat(MAX_PAGE_TEXT_CHARS + 7);
        let read = page_text(None, &json!(long), "http://localhost:3000/".into());
        assert!(read.as_ref().is_ok_and(|r| r.truncated));
        assert_eq!(
            read.as_ref().map(|r| r.text.chars().count()),
            Ok(MAX_PAGE_TEXT_CHARS)
        );
        assert_eq!(read.map(|r| r.chars), Ok(MAX_PAGE_TEXT_CHARS + 7));
        let short = page_text(Some("h1"), &json!("Hello"), "u".into());
        assert_eq!(
            short,
            Ok(PageText {
                text: "Hello".into(),
                truncated: false,
                chars: 5,
                url: "u".into()
            })
        );
        assert!(matches!(
            page_text(Some("#none"), &Value::Null, "u".into()),
            Err(BrowserError::Element { selector, .. }) if selector == "#none"
        ));
    }
}
