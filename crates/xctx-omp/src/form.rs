//! omp's `ask` tool as a form: parse the HTML it sends, check answers the way omp does.
//!
//! omp sends `<form>` HTML (`extension_ui_request` method `form`) and wants a map of answers back. The markup is a
//! small, regular subset: `fieldset`/`legend`, `label`, `input` (text, number, range, email, url, hidden, radio,
//! checkbox), `select`/`option`, `textarea`. [`Form::parse`] reads that subset into fields; [`Form::answer`] turns
//! the user's [`Answers`] into the `data` map, or says which fields fail. An answer omp would reject is never
//! produced here.
//!
//! Answer shapes follow `omp://rpc.md`: a radio group, a single `select` or a lone checkbox gives one string; a
//! checkbox group or a multi-`select` gives an array; a number or range gives a number. A choice group of two or
//! more options with no option labelled "Other" also takes one free-text answer.

use std::collections::BTreeMap;

use regex::Regex;
use serde_json::{Map, Number, Value};

/// A parsed form.
#[derive(Debug, Clone, PartialEq)]
pub struct Form {
    /// Notes and fields in the order the form shows them.
    pub items: Vec<Item>,
    /// The fields, by `Item::Field` index.
    pub fields: Vec<Field>,
}

/// One thing the form shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// Text that is not a label (a heading or paragraph).
    Note(String),
    /// A field, an index into [`Form::fields`].
    Field(usize),
}

/// A field the user fills in.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// The key in the answer map.
    pub name: String,
    /// What to show: the `legend` or label, else the name.
    pub label: String,
    /// Whether an answer is needed.
    pub required: bool,
    /// What kind of field it is.
    pub kind: FieldKind,
}

/// The kinds of field.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    /// Free text or a number.
    Text(TextField),
    /// Pick from options.
    Choice(ChoiceField),
    /// A value the form carries and the user never sees.
    Hidden(String),
}

/// The flavours of text field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    /// One line.
    Text,
    /// Several lines (`textarea`).
    Multiline,
    /// A number (`type=number`).
    Number,
    /// A number on a slider (`type=range`).
    Range,
    /// An email address.
    Email,
    /// A URL.
    Url,
}

/// A text-like field with its constraints.
#[derive(Debug, Clone, PartialEq)]
pub struct TextField {
    /// The flavour.
    pub kind: TextKind,
    /// The starting value.
    pub value: String,
    /// The hint shown while empty.
    pub placeholder: String,
    /// Lowest allowed number.
    pub min: Option<f64>,
    /// Highest allowed number.
    pub max: Option<f64>,
    /// Fewest characters.
    pub min_length: Option<usize>,
    /// Most characters.
    pub max_length: Option<usize>,
    /// A pattern the whole value must match.
    pub pattern: Option<String>,
}

/// How a choice field is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceControl {
    /// Radio buttons.
    Radio,
    /// Check boxes.
    Checkbox,
    /// A drop-down.
    Select,
}

/// A choice field.
#[derive(Debug, Clone, PartialEq)]
pub struct ChoiceField {
    /// How it is drawn.
    pub control: ChoiceControl,
    /// More than one may be chosen.
    pub multiple: bool,
    /// The options.
    pub options: Vec<ChoiceOption>,
    /// Whether a free-text answer outside the options is allowed.
    pub other: bool,
    /// The answer is an array (a group of two or more checkboxes, or a multi-select of two or more options).
    pub array: bool,
}

/// One option of a choice field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceOption {
    /// The value sent back.
    pub value: String,
    /// What to show.
    pub label: String,
    /// Chosen to begin with.
    pub checked: bool,
    /// Not choosable.
    pub disabled: bool,
}

/// What the user answered.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Answers {
    /// Text, by field name.
    pub text: BTreeMap<String, String>,
    /// Chosen option values, by field name.
    pub chosen: BTreeMap<String, Vec<String>>,
    /// The free-text "Other" answer, by field name.
    pub other: BTreeMap<String, String>,
}

impl Answers {
    /// The form's own defaults: starting text and pre-checked options.
    pub fn defaults(form: &Form) -> Answers {
        let mut a = Answers::default();
        for f in &form.fields {
            match &f.kind {
                FieldKind::Text(t) => {
                    a.text.insert(f.name.clone(), t.value.clone());
                }
                FieldKind::Choice(c) => {
                    let on: Vec<String> = c
                        .options
                        .iter()
                        .filter(|o| o.checked && !o.disabled)
                        .map(|o| o.value.clone())
                        .collect();
                    if !on.is_empty() {
                        a.chosen.insert(f.name.clone(), on);
                    }
                }
                FieldKind::Hidden(_) => {}
            }
        }
        a
    }
}

/// A field that cannot be sent as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    /// The field's name.
    pub name: String,
    /// What is wrong, in words for the user.
    pub message: String,
}

impl Form {
    /// Read the form omp sent. Never fails: markup it does not know is skipped.
    pub fn parse(html: &str) -> Form {
        Parser::new(html).run()
    }

    /// Turn answers into the `data` map, or list every field that fails.
    pub fn answer(&self, answers: &Answers) -> Result<Map<String, Value>, Vec<FieldError>> {
        let mut data = Map::new();
        let mut errors = Vec::new();
        for field in &self.fields {
            let mut fail = |message: &str| {
                errors.push(FieldError {
                    name: field.name.clone(),
                    message: message.to_owned(),
                });
            };
            match &field.kind {
                FieldKind::Hidden(v) => {
                    data.insert(field.name.clone(), Value::String(v.clone()));
                }
                FieldKind::Text(t) => {
                    let raw = answers.text.get(&field.name).map_or("", String::as_str);
                    match check_text(field.required, t, raw) {
                        Ok(Some(v)) => {
                            data.insert(field.name.clone(), v);
                        }
                        Ok(None) => {}
                        Err(m) => fail(&m),
                    }
                }
                FieldKind::Choice(c) => {
                    let chosen = answers
                        .chosen
                        .get(&field.name)
                        .map_or(&[][..], Vec::as_slice);
                    let other = answers
                        .other
                        .get(&field.name)
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty());
                    match check_choice(field.required, c, chosen, other) {
                        Ok(Some(v)) => {
                            data.insert(field.name.clone(), v);
                        }
                        Ok(None) => {}
                        Err(m) => fail(&m),
                    }
                }
            }
        }
        if errors.is_empty() {
            Ok(data)
        } else {
            Err(errors)
        }
    }
}

fn check_text(required: bool, t: &TextField, raw: &str) -> Result<Option<Value>, String> {
    if raw.trim().is_empty() {
        return if required {
            Err("Required".to_owned())
        } else if matches!(t.kind, TextKind::Number | TextKind::Range) {
            Ok(None)
        } else {
            Ok(Some(Value::String(String::new())))
        };
    }
    let len = raw.chars().count();
    if let Some(min) = t.min_length
        && len < min
    {
        return Err(format!("At least {min} characters"));
    }
    if let Some(max) = t.max_length
        && len > max
    {
        return Err(format!("At most {max} characters"));
    }
    match t.kind {
        TextKind::Number | TextKind::Range => {
            let n: f64 = raw
                .trim()
                .parse()
                .map_err(|_| "Enter a number".to_owned())?;
            if !n.is_finite() {
                return Err("Enter a number".to_owned());
            }
            if let Some(min) = t.min
                && n < min
            {
                return Err(format!("At least {min}"));
            }
            if let Some(max) = t.max
                && n > max
            {
                return Err(format!("At most {max}"));
            }
            Ok(Some(number(n)))
        }
        TextKind::Email if !EMAIL.is_match(raw.trim()) => Err("Enter an email address".to_owned()),
        TextKind::Url if !URL.is_match(raw.trim()) => Err("Enter a URL".to_owned()),
        _ => {
            if let Some(p) = &t.pattern
                && let Ok(re) = Regex::new(&format!("^(?:{p})$"))
                && !re.is_match(raw)
            {
                return Err("Does not match the expected format".to_owned());
            }
            let value = if matches!(t.kind, TextKind::Email | TextKind::Url) {
                raw.trim()
            } else {
                raw
            };
            Ok(Some(Value::String(value.to_owned())))
        }
    }
}

fn number(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9e15 {
        Value::Number(Number::from(n as i64))
    } else {
        Number::from_f64(n).map_or(Value::Null, Value::Number)
    }
}

fn check_choice(
    required: bool,
    c: &ChoiceField,
    chosen: &[String],
    other: Option<&str>,
) -> Result<Option<Value>, String> {
    for v in chosen {
        match c.options.iter().find(|o| &o.value == v) {
            None => return Err(format!("\"{v}\" is not one of the options")),
            Some(o) if o.disabled => return Err(format!("\"{}\" cannot be chosen", o.label)),
            Some(_) => {}
        }
    }
    if other.is_some() && !c.other {
        return Err("Other is not allowed here".to_owned());
    }
    if let Some(o) = other
        && c.options.iter().any(|opt| opt.value == o)
    {
        return Err("Pick that option instead of typing it".to_owned());
    }
    let count = chosen.len() + usize::from(other.is_some());
    if count == 0 {
        return if required {
            Err("Choose one".to_owned())
        } else if c.array {
            Ok(Some(Value::Array(Vec::new())))
        } else {
            Ok(None)
        };
    }
    if !c.multiple && count > 1 {
        return Err("Choose only one".to_owned());
    }
    let mut all: Vec<Value> = chosen.iter().cloned().map(Value::String).collect();
    if let Some(o) = other {
        all.push(Value::String(o.to_owned()));
    }
    Ok(Some(if c.array {
        Value::Array(all)
    } else {
        all.remove(0)
    }))
}

static EMAIL_SRC: &str = r"^[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+@[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?(?:\.[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?)*$";
static URL_SRC: &str = r"^[A-Za-z][A-Za-z0-9+.\-]*:\S+$";

struct Lazy(&'static str, std::sync::OnceLock<Regex>);

impl Lazy {
    fn is_match(&self, s: &str) -> bool {
        self.1
            .get_or_init(|| Regex::new(self.0).expect("static pattern"))
            .is_match(s)
    }
}

static EMAIL: Lazy = Lazy(EMAIL_SRC, std::sync::OnceLock::new());
static URL: Lazy = Lazy(URL_SRC, std::sync::OnceLock::new());

// ---- HTML reading -------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Token {
    Open(String, Vec<(String, String)>),
    Close(String),
    Text(String),
}

fn tokenize(html: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let b = html.as_bytes();
    let mut i = 0;
    let mut text_start = 0;
    let flush = |out: &mut Vec<Token>, from: usize, to: usize| {
        if to > from {
            out.push(Token::Text(decode(&html[from..to])));
        }
    };
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        if html[i..].starts_with("<!--") {
            flush(&mut out, text_start, i);
            i = html[i..].find("-->").map_or(b.len(), |e| i + e + 3);
            text_start = i;
            continue;
        }
        let Some(end) = html[i..].find('>').map(|e| i + e) else {
            i += 1;
            continue;
        };
        let inner = &html[i + 1..end];
        let starts_tag = inner
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '/' || c == '!');
        if !starts_tag {
            i += 1;
            continue;
        }
        flush(&mut out, text_start, i);
        if let Some(name) = inner.strip_prefix('/') {
            out.push(Token::Close(name.trim().to_ascii_lowercase()));
        } else if !inner.starts_with('!') {
            let (name, attrs) = parse_tag(inner.trim_end_matches('/'));
            out.push(Token::Open(name, attrs));
        }
        i = end + 1;
        text_start = i;
    }
    flush(&mut out, text_start, b.len());
    out
}

fn parse_tag(inner: &str) -> (String, Vec<(String, String)>) {
    let name_end = inner
        .find(|c: char| c.is_whitespace())
        .unwrap_or(inner.len());
    let name = inner[..name_end].to_ascii_lowercase();
    let mut attrs = Vec::new();
    let rest: Vec<char> = inner[name_end..].chars().collect();
    let mut i = 0;
    while i < rest.len() {
        while i < rest.len() && rest[i].is_whitespace() {
            i += 1;
        }
        let start = i;
        while i < rest.len() && !rest[i].is_whitespace() && rest[i] != '=' {
            i += 1;
        }
        if start == i {
            i += 1;
            continue;
        }
        let key: String = rest[start..i]
            .iter()
            .collect::<String>()
            .to_ascii_lowercase();
        while i < rest.len() && rest[i].is_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < rest.len() && rest[i] == '=' {
            i += 1;
            while i < rest.len() && rest[i].is_whitespace() {
                i += 1;
            }
            if i < rest.len() && (rest[i] == '"' || rest[i] == '\'') {
                let q = rest[i];
                i += 1;
                let s = i;
                while i < rest.len() && rest[i] != q {
                    i += 1;
                }
                value = rest[s..i].iter().collect();
                i += 1;
            } else {
                let s = i;
                while i < rest.len() && !rest[i].is_whitespace() {
                    i += 1;
                }
                value = rest[s..i].iter().collect();
            }
        }
        attrs.push((key, decode(&value)));
    }
    (name, attrs)
}

fn decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(semi) = rest.find(';').filter(|s| *s <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..semi];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => ent
                .strip_prefix('#')
                .and_then(|n| {
                    n.strip_prefix(['x', 'X'])
                        .map_or_else(|| n.parse().ok(), |h| u32::from_str_radix(h, 16).ok())
                })
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn has(attrs: &[(String, String)], key: &str) -> bool {
    attrs.iter().any(|(k, _)| k == key)
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A control as read, before controls with one name are grouped.
#[derive(Debug, Clone)]
struct Raw {
    name: String,
    id: Option<String>,
    kind: RawKind,
    required: bool,
    legend: Option<String>,
    label: String,
}

#[derive(Debug, Clone)]
enum RawKind {
    Text(TextField),
    Radio(ChoiceOption),
    Checkbox(ChoiceOption),
    Select {
        multiple: bool,
        options: Vec<ChoiceOption>,
    },
    Hidden(String),
}

struct Parser {
    tokens: Vec<Token>,
}

impl Parser {
    fn new(html: &str) -> Self {
        Parser {
            tokens: tokenize(html),
        }
    }

    fn run(self) -> Form {
        let mut raws: Vec<Raw> = Vec::new();
        let mut notes: Vec<(usize, String)> = Vec::new();
        let mut legends: Vec<Option<String>> = Vec::new();
        let mut in_legend: Option<String> = None;
        let mut label: Option<(String, Vec<usize>, Option<String>)> = None;
        let mut for_labels: Vec<(String, String)> = Vec::new();
        let mut select: Option<(Raw, bool, Vec<ChoiceOption>)> = None;
        let mut option: Option<(ChoiceOption, String, bool)> = None;
        let mut area: Option<(Raw, String)> = None;
        let mut skip = 0u32;

        for token in self.tokens {
            if skip > 0 {
                if matches!(&token, Token::Close(n) if n == "script" || n == "style") {
                    skip -= 1;
                }
                continue;
            }
            match token {
                Token::Open(name, attrs) => match name.as_str() {
                    "script" | "style" => skip += 1,
                    "fieldset" => legends.push(None),
                    "legend" => in_legend = Some(String::new()),
                    "label" => {
                        label = Some((
                            String::new(),
                            Vec::new(),
                            attr(&attrs, "for").map(str::to_owned),
                        ));
                    }
                    "input" => {
                        let ty = attr(&attrs, "type").unwrap_or("text").to_ascii_lowercase();
                        let value = attr(&attrs, "value").unwrap_or("").to_owned();
                        let kind = match ty.as_str() {
                            "hidden" => RawKind::Hidden(value),
                            "radio" | "checkbox" => {
                                let opt = ChoiceOption {
                                    value: attr(&attrs, "value").unwrap_or("on").to_owned(),
                                    label: String::new(),
                                    checked: has(&attrs, "checked"),
                                    disabled: has(&attrs, "disabled"),
                                };
                                if ty == "radio" {
                                    RawKind::Radio(opt)
                                } else {
                                    RawKind::Checkbox(opt)
                                }
                            }
                            "submit" | "button" | "reset" | "image" | "file" => continue,
                            other => RawKind::Text(text_field(
                                match other {
                                    "number" => TextKind::Number,
                                    "range" => TextKind::Range,
                                    "email" => TextKind::Email,
                                    "url" => TextKind::Url,
                                    _ => TextKind::Text,
                                },
                                &attrs,
                                value,
                            )),
                        };
                        let Some(name) = attr(&attrs, "name").filter(|n| !n.is_empty()) else {
                            continue;
                        };
                        let raw = Raw {
                            name: name.to_owned(),
                            id: attr(&attrs, "id").map(str::to_owned),
                            kind,
                            required: has(&attrs, "required"),
                            legend: current_legend(&legends),
                            label: String::new(),
                        };
                        if let Some((_, members, _)) = label.as_mut() {
                            members.push(raws.len());
                        }
                        raws.push(raw);
                    }
                    "select" => {
                        if let Some(name) = attr(&attrs, "name").filter(|n| !n.is_empty()) {
                            let raw = Raw {
                                name: name.to_owned(),
                                id: attr(&attrs, "id").map(str::to_owned),
                                kind: RawKind::Select {
                                    multiple: has(&attrs, "multiple"),
                                    options: Vec::new(),
                                },
                                required: has(&attrs, "required"),
                                legend: current_legend(&legends),
                                label: String::new(),
                            };
                            if let Some((_, members, _)) = label.as_mut() {
                                members.push(raws.len());
                            }
                            select = Some((raw, has(&attrs, "multiple"), Vec::new()));
                        }
                    }
                    "option" if select.is_some() => {
                        option = Some((
                            ChoiceOption {
                                value: attr(&attrs, "value").unwrap_or("").to_owned(),
                                label: String::new(),
                                checked: has(&attrs, "selected"),
                                disabled: has(&attrs, "disabled"),
                            },
                            String::new(),
                            has(&attrs, "value"),
                        ));
                    }
                    "textarea" => {
                        if let Some(name) = attr(&attrs, "name").filter(|n| !n.is_empty()) {
                            let raw = Raw {
                                name: name.to_owned(),
                                id: attr(&attrs, "id").map(str::to_owned),
                                kind: RawKind::Text(text_field(
                                    TextKind::Multiline,
                                    &attrs,
                                    String::new(),
                                )),
                                required: has(&attrs, "required"),
                                legend: current_legend(&legends),
                                label: String::new(),
                            };
                            if let Some((_, members, _)) = label.as_mut() {
                                members.push(raws.len());
                            }
                            area = Some((raw, String::new()));
                        }
                    }
                    _ => {}
                },
                Token::Close(name) => match name.as_str() {
                    "fieldset" => {
                        legends.pop();
                    }
                    "legend" => {
                        if let Some(text) = in_legend.take()
                            && let Some(slot) = legends.last_mut()
                        {
                            *slot = Some(squash(&text));
                        }
                    }
                    "label" => {
                        if let Some((text, members, target)) = label.take() {
                            let text = squash(&text);
                            for m in &members {
                                raws[*m].label.clone_from(&text);
                            }
                            if let Some(t) = target {
                                for_labels.push((t, text));
                            }
                        }
                    }
                    "option" => {
                        if let (Some((mut opt, text, explicit)), Some((_, _, opts))) =
                            (option.take(), select.as_mut())
                        {
                            opt.label = squash(&text);
                            if !explicit {
                                opt.value.clone_from(&opt.label);
                            }
                            opts.push(opt);
                        }
                    }
                    "select" => {
                        if let Some((mut raw, multiple, options)) = select.take() {
                            raw.kind = RawKind::Select { multiple, options };
                            raws.push(raw);
                        }
                    }
                    "textarea" => {
                        if let Some((mut raw, text)) = area.take() {
                            if let RawKind::Text(t) = &mut raw.kind {
                                t.value = text.trim_start_matches('\n').to_owned();
                            }
                            raws.push(raw);
                        }
                    }
                    _ => {}
                },
                Token::Text(text) => {
                    if let Some((_, buf)) = area.as_mut() {
                        buf.push_str(&text);
                    } else if let Some((_, buf, _)) = option.as_mut() {
                        buf.push_str(&text);
                    } else if let Some(buf) = in_legend.as_mut() {
                        buf.push_str(&text);
                    } else if let Some((buf, _, _)) = label.as_mut() {
                        buf.push_str(&text);
                    } else if select.is_none() {
                        let t = squash(&text);
                        if !t.is_empty() {
                            notes.push((raws.len(), t));
                        }
                    }
                }
            }
        }

        for (target, text) in for_labels {
            for raw in raws.iter_mut().filter(|r| r.id.as_deref() == Some(&target)) {
                raw.label.clone_from(&text);
            }
        }
        assemble(raws, notes)
    }
}

fn current_legend(stack: &[Option<String>]) -> Option<String> {
    stack.iter().rev().find_map(Clone::clone)
}

fn text_field(kind: TextKind, attrs: &[(String, String)], value: String) -> TextField {
    let num = |k: &str| attr(attrs, k).and_then(|v| v.trim().parse::<f64>().ok());
    let len = |k: &str| attr(attrs, k).and_then(|v| v.trim().parse::<usize>().ok());
    TextField {
        kind,
        value,
        placeholder: attr(attrs, "placeholder").unwrap_or("").to_owned(),
        min: num("min"),
        max: num("max"),
        min_length: len("minlength"),
        max_length: len("maxlength"),
        pattern: attr(attrs, "pattern").map(str::to_owned),
    }
}

fn assemble(raws: Vec<Raw>, notes: Vec<(usize, String)>) -> Form {
    let mut fields: Vec<Field> = Vec::new();
    // First raw index of each field, so notes can be placed before the field that follows them.
    let mut first_raw: Vec<usize> = Vec::new();

    for (i, raw) in raws.iter().enumerate() {
        let existing = fields.iter().position(|f| {
            f.name == raw.name
                && matches!(
                    (&f.kind, &raw.kind),
                    (
                        FieldKind::Choice(ChoiceField {
                            control: ChoiceControl::Radio,
                            ..
                        }),
                        RawKind::Radio(_)
                    ) | (
                        FieldKind::Choice(ChoiceField {
                            control: ChoiceControl::Checkbox,
                            ..
                        }),
                        RawKind::Checkbox(_)
                    )
                )
        });
        match (&raw.kind, existing) {
            (RawKind::Radio(opt) | RawKind::Checkbox(opt), Some(at)) => {
                let mut opt = opt.clone();
                opt.label = option_label(&raw.label, &opt.value);
                if let FieldKind::Choice(c) = &mut fields[at].kind {
                    c.options.push(opt);
                }
                fields[at].required |= raw.required;
            }
            (RawKind::Radio(opt) | RawKind::Checkbox(opt), None) => {
                let radio = matches!(raw.kind, RawKind::Radio(_));
                let mut opt = opt.clone();
                opt.label = option_label(&raw.label, &opt.value);
                fields.push(Field {
                    name: raw.name.clone(),
                    label: raw.legend.clone().unwrap_or_else(|| raw.name.clone()),
                    required: raw.required,
                    kind: FieldKind::Choice(ChoiceField {
                        control: if radio {
                            ChoiceControl::Radio
                        } else {
                            ChoiceControl::Checkbox
                        },
                        multiple: !radio,
                        options: vec![opt],
                        other: false,
                        array: false,
                    }),
                });
                first_raw.push(i);
            }
            (RawKind::Select { multiple, options }, _) => {
                fields.push(Field {
                    name: raw.name.clone(),
                    label: field_label(raw),
                    required: raw.required,
                    kind: FieldKind::Choice(ChoiceField {
                        control: ChoiceControl::Select,
                        multiple: *multiple,
                        options: options.clone(),
                        other: false,
                        array: false,
                    }),
                });
                first_raw.push(i);
            }
            (RawKind::Text(t), _) => {
                fields.push(Field {
                    name: raw.name.clone(),
                    label: field_label(raw),
                    required: raw.required,
                    kind: FieldKind::Text(t.clone()),
                });
                first_raw.push(i);
            }
            (RawKind::Hidden(v), _) => {
                fields.push(Field {
                    name: raw.name.clone(),
                    label: raw.name.clone(),
                    required: false,
                    kind: FieldKind::Hidden(v.clone()),
                });
                first_raw.push(i);
            }
        }
    }

    for f in &mut fields {
        if let FieldKind::Choice(c) = &mut f.kind {
            let many = c.options.len() >= 2;
            let has_other_option = c.options.iter().any(|o| {
                o.label.eq_ignore_ascii_case("other") || o.value.eq_ignore_ascii_case("other")
            });
            c.other = many && !has_other_option && c.control != ChoiceControl::Select;
            c.array = c.multiple && many;
            if !c.multiple && c.options.iter().filter(|o| o.checked).count() > 1 {
                let mut seen = false;
                for o in &mut c.options {
                    if o.checked {
                        o.checked = !seen;
                        seen = true;
                    }
                }
            }
        }
    }

    let mut items = Vec::new();
    let mut pending = notes.into_iter().peekable();
    for (idx, raw_at) in first_raw.iter().enumerate() {
        while let Some((_, text)) = pending.next_if(|(slot, _)| *slot <= *raw_at) {
            items.push(Item::Note(text));
        }
        if !matches!(fields[idx].kind, FieldKind::Hidden(_)) {
            items.push(Item::Field(idx));
        }
    }
    for (_, text) in pending {
        items.push(Item::Note(text));
    }
    Form { items, fields }
}

fn option_label(label: &str, value: &str) -> String {
    if label.is_empty() {
        value.to_owned()
    } else {
        label.to_owned()
    }
}

fn field_label(raw: &Raw) -> String {
    if !raw.label.is_empty() {
        raw.label.clone()
    } else if let Some(l) = &raw.legend {
        l.clone()
    } else if let RawKind::Text(t) = &raw.kind
        && !t.placeholder.is_empty()
    {
        t.placeholder.clone()
    } else {
        raw.name.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ASK: &str = r#"<form><fieldset><legend>Which colour do you prefer?</legend><label><input type="radio" name="colour" value="red"> Red</label><label><input type="radio" name="colour" value="blue"> Blue</label></fieldset></form>"#;

    #[test]
    fn parses_the_recorded_radio_group() {
        let form = Form::parse(ASK);
        assert_eq!(form.fields.len(), 1);
        let f = &form.fields[0];
        assert_eq!(f.name, "colour");
        assert_eq!(f.label, "Which colour do you prefer?");
        let FieldKind::Choice(c) = &f.kind else {
            panic!("choice")
        };
        assert_eq!(c.control, ChoiceControl::Radio);
        assert_eq!(
            c.options
                .iter()
                .map(|o| o.label.as_str())
                .collect::<Vec<_>>(),
            ["Red", "Blue"]
        );
        assert!(c.other, "two options with no Other allow free text");
        assert!(!c.array);
    }

    #[test]
    fn answers_a_radio_group_with_one_string() {
        let form = Form::parse(ASK);
        let mut a = Answers::default();
        a.chosen.insert("colour".into(), vec!["blue".into()]);
        assert_eq!(form.answer(&a).unwrap().get("colour"), Some(&json!("blue")));
    }

    #[test]
    fn radio_group_takes_other_text() {
        let form = Form::parse(ASK);
        let mut a = Answers::default();
        a.other.insert("colour".into(), "green".into());
        assert_eq!(
            form.answer(&a).unwrap().get("colour"),
            Some(&json!("green"))
        );
        a.chosen.insert("colour".into(), vec!["red".into()]);
        assert!(form.answer(&a).is_err(), "one radio only");
    }

    #[test]
    fn checkbox_group_gives_an_array_even_when_empty() {
        let form = Form::parse(
            r#"<form><fieldset><legend>Pick</legend>
            <label><input type="checkbox" name="t" value="a"> A</label>
            <label><input type="checkbox" name="t" value="b" checked> B</label></fieldset></form>"#,
        );
        let a = Answers::defaults(&form);
        assert_eq!(form.answer(&a).unwrap().get("t"), Some(&json!(["b"])));
        assert_eq!(
            form.answer(&Answers::default()).unwrap().get("t"),
            Some(&json!([]))
        );
    }

    #[test]
    fn lone_checkbox_is_one_value_and_absent_when_unticked() {
        let form = Form::parse(
            r#"<form><label><input type="checkbox" name="ok" value="yes"> Sure</label></form>"#,
        );
        assert!(
            form.answer(&Answers::default())
                .unwrap()
                .get("ok")
                .is_none()
        );
        let mut a = Answers::default();
        a.chosen.insert("ok".into(), vec!["yes".into()]);
        assert_eq!(form.answer(&a).unwrap().get("ok"), Some(&json!("yes")));
    }

    #[test]
    fn required_and_constraints_are_checked() {
        let form = Form::parse(
            r#"<form>
            <label>Name <input name="n" required minlength="3" maxlength="5"></label>
            <label>Age <input type="number" name="age" min="1" max="120"></label>
            <label>Code <input name="code" pattern="[A-Z]{2}[0-9]"></label>
            <label>Mail <input type="email" name="mail"></label></form>"#,
        );
        assert_eq!(form.fields[0].label, "Name");
        let mut a = Answers::default();
        let errs = form.answer(&a).unwrap_err();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].name, "n");

        a.text.insert("n".into(), "ab".into());
        assert!(form.answer(&a).is_err());
        a.text.insert("n".into(), "abcd".into());
        a.text.insert("age".into(), "200".into());
        a.text.insert("code".into(), "AB12".into());
        a.text.insert("mail".into(), "nope".into());
        let names: Vec<_> = form
            .answer(&a)
            .unwrap_err()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["age", "code", "mail"]);

        a.text.insert("age".into(), "42".into());
        a.text.insert("code".into(), "AB1".into());
        a.text.insert("mail".into(), "me@example.com".into());
        let data = form.answer(&a).unwrap();
        assert_eq!(data.get("age"), Some(&json!(42)));
        assert_eq!(data.get("code"), Some(&json!("AB1")));
    }

    #[test]
    fn a_blank_optional_number_is_left_out() {
        let form = Form::parse(r#"<form><input type="number" name="n"></form>"#);
        assert!(form.answer(&Answers::default()).unwrap().get("n").is_none());
    }

    #[test]
    fn select_and_textarea_and_hidden() {
        let form = Form::parse(
            r#"<form><input type="hidden" name="h" value="v">
            <select name="s"><option value="x" selected>X</option><option>Y</option><option value="z" disabled>Z</option></select>
            <textarea name="t">&lt;hi&gt;</textarea></form>"#,
        );
        let mut a = Answers::defaults(&form);
        let data = form.answer(&a).unwrap();
        assert_eq!(data.get("h"), Some(&json!("v")));
        assert_eq!(data.get("s"), Some(&json!("x")));
        assert_eq!(data.get("t"), Some(&json!("<hi>")));
        a.chosen.insert("s".into(), vec!["Y".into()]);
        assert_eq!(form.answer(&a).unwrap().get("s"), Some(&json!("Y")));
        a.chosen.insert("s".into(), vec!["z".into()]);
        assert!(form.answer(&a).is_err(), "disabled option");
        assert_eq!(form.items.len(), 2, "hidden fields are not shown");
    }

    #[test]
    fn unknown_markup_and_scripts_are_skipped() {
        let form = Form::parse(
            "<form><h2>Hi &amp; bye</h2><script>if (a < b) {}</script><button>Go</button><input name=q></form>",
        );
        assert_eq!(form.items[0], Item::Note("Hi & bye".into()));
        assert_eq!(form.fields.len(), 1);
    }
}
