//! The Jira connector: `issue.read` and `issue.comment` through Jira Cloud's REST API v3, with basic auth
//! (`JIRA_EMAIL` and `JIRA_API_TOKEN`) against `JIRA_BASE_URL`.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value, json};
use xctx_core::Redactor;
use xctx_http::Request;

use crate::args::Args;
use crate::error::{ConnectorError, Result};
use crate::rest::{self, flatten_adf, text_to_adf};
use crate::{BuiltinContext, BuiltinOutput};

/// The most recent comments `issue.read` returns.
const MAX_COMMENTS: usize = 10;

/// The longest comment `issue.comment` posts.
const MAX_COMMENT_CHARS: usize = 32 * 1024;

static KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:[A-Za-z][A-Za-z0-9_]*-[0-9]+|[0-9]+)$")
        .unwrap_or_else(|e| panic!("bad built-in regex: {e}"))
});

struct Jira {
    base: String,
    auth: String,
}

fn connect(ctx: &BuiltinContext, masker: &mut Redactor) -> Result<Jira> {
    let missing = |name: &str| {
        ConnectorError::NotConfigured(format!(
            "{name} is not set in the space's environment or secrets"
        ))
    };
    let base = rest::base_url(
        ctx.setting("JIRA_BASE_URL")
            .ok_or_else(|| missing("JIRA_BASE_URL"))?,
    )?;
    let email = ctx
        .setting("JIRA_EMAIL")
        .ok_or_else(|| missing("JIRA_EMAIL"))?;
    let token = ctx
        .setting("JIRA_API_TOKEN")
        .ok_or_else(|| missing("JIRA_API_TOKEN"))?;
    let auth = rest::basic_auth(email, token);
    masker.extend([
        ("JIRA_API_TOKEN".to_owned(), token.to_owned()),
        ("JIRA_API_TOKEN".to_owned(), auth.clone()),
    ]);
    Ok(Jira {
        base,
        auth: format!("Basic {auth}"),
    })
}

fn issue_key(a: &Args<'_>) -> Result<String> {
    let key = a.required_str("key")?.trim();
    if !KEY.is_match(key) {
        return Err(ConnectorError::BadArgs(format!(
            "{key:?} is not a Jira issue key such as PROJ-123"
        )));
    }
    Ok(key.to_ascii_uppercase())
}

fn str_at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = v;
    for p in path {
        cur = cur.get(*p)?;
    }
    cur.as_str()
}

/// `issue.read`: JSON `{key, url, summary, status, type, priority, assignee, reporter, labels, updated,
/// description, comments: [{author, created, body}], comment_total}`, with the description and comments as plain
/// text.
pub(crate) fn issue_read(
    args: &Map<String, Value>,
    ctx: &BuiltinContext,
    masker: &mut Redactor,
) -> Result<BuiltinOutput> {
    let a = Args::new("issue.read", args, &["key"])?;
    let key = issue_key(&a)?;
    let jira = connect(ctx, masker)?;
    let options = rest::options(&jira.base, ctx)?;
    let url = format!(
        "{}/rest/api/3/issue/{key}?fields=summary,status,assignee,reporter,priority,issuetype,labels,updated,description,comment",
        jira.base
    );
    let request = Request::new("GET", url)
        .header("Authorization", &jira.auth)
        .header("Accept", "application/json");
    let issue = rest::send_json("Jira", &request, &options)?;

    let fields = issue.get("fields").cloned().unwrap_or(Value::Null);
    let key = issue
        .get("key")
        .and_then(Value::as_str)
        .unwrap_or(&key)
        .to_owned();
    let summary = str_at(&fields, &["summary"]).unwrap_or("").to_owned();
    let status = str_at(&fields, &["status", "name"]);
    let kind = str_at(&fields, &["issuetype", "name"]);
    let priority = str_at(&fields, &["priority", "name"]);
    let assignee = str_at(&fields, &["assignee", "displayName"]);
    let reporter = str_at(&fields, &["reporter", "displayName"]);
    let updated = str_at(&fields, &["updated"]);
    let labels: Vec<&str> = fields
        .get("labels")
        .and_then(Value::as_array)
        .map(|l| l.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let description = fields
        .get("description")
        .map(flatten_adf)
        .unwrap_or_default();
    let all_comments: &[Value] = fields
        .get("comment")
        .and_then(|c| c.get("comments"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let comment_total = fields
        .get("comment")
        .and_then(|c| c.get("total"))
        .and_then(Value::as_u64)
        .unwrap_or(all_comments.len() as u64);
    let comments: Vec<Value> = all_comments[all_comments.len().saturating_sub(MAX_COMMENTS)..]
        .iter()
        .map(|c| {
            json!({
                "author": str_at(c, &["author", "displayName"]),
                "created": str_at(c, &["created"]),
                "body": c.get("body").map(flatten_adf).unwrap_or_default(),
            })
        })
        .collect();
    let browse = format!("{}/browse/{key}", jira.base);

    let mut text = format!("{key}: {summary}\n");
    let facts: Vec<String> = [
        ("Status", status),
        ("Type", kind),
        ("Priority", priority),
        ("Assignee", Some(assignee.unwrap_or("unassigned"))),
        ("Reporter", reporter),
    ]
    .iter()
    .filter_map(|(n, v)| v.map(|v| format!("{n}: {v}")))
    .collect();
    text.push_str(&facts.join(" · "));
    text.push('\n');
    if !labels.is_empty() {
        text.push_str(&format!("Labels: {}\n", labels.join(", ")));
    }
    text.push_str(&browse);
    text.push_str("\n\n");
    text.push_str(if description.is_empty() {
        "(no description)"
    } else {
        &description
    });
    text.push('\n');
    if !comments.is_empty() {
        text.push_str(&format!(
            "\nComments ({comment_total}, the last {} shown):\n",
            comments.len()
        ));
        for c in &comments {
            text.push_str(&format!(
                "\n--- {}, {}\n{}\n",
                c["author"].as_str().unwrap_or("someone"),
                c["created"].as_str().unwrap_or(""),
                c["body"].as_str().unwrap_or("")
            ));
        }
    }

    let json = json!({
        "key": key,
        "url": browse,
        "summary": summary,
        "status": status,
        "type": kind,
        "priority": priority,
        "assignee": assignee,
        "reporter": reporter,
        "labels": labels,
        "updated": updated,
        "description": description,
        "comments": comments,
        "comment_total": comment_total,
    });
    Ok(BuiltinOutput {
        json,
        text,
        events: Vec::new(),
    })
}

/// `issue.comment`: posts `body` as a comment (plain text turned into ADF paragraphs). JSON `{key, id, created,
/// url}`. The caller has checked approval.
pub(crate) fn issue_comment(
    args: &Map<String, Value>,
    ctx: &BuiltinContext,
    masker: &mut Redactor,
) -> Result<BuiltinOutput> {
    let a = Args::new("issue.comment", args, &["key", "body"])?;
    let key = issue_key(&a)?;
    let body = a.required_str("body")?;
    if body.chars().count() > MAX_COMMENT_CHARS {
        return Err(ConnectorError::BadArgs(format!(
            "issue.comment: the body is longer than {MAX_COMMENT_CHARS} characters"
        )));
    }
    let jira = connect(ctx, masker)?;
    let options = rest::options(&jira.base, ctx)?;
    let url = format!("{}/rest/api/3/issue/{key}/comment", jira.base);
    let payload = json!({ "body": text_to_adf(body) });
    let request = Request::new("POST", url)
        .header("Authorization", &jira.auth)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .body(payload.to_string());
    let answer = rest::send_json("Jira", &request, &options)?;
    let id = answer.get("id").and_then(Value::as_str).map(str::to_owned);
    let created = answer
        .get("created")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let browse = format!("{}/browse/{key}", jira.base);
    let text = match &id {
        Some(id) => format!("Commented on {key} (comment {id}): {browse}"),
        None => format!("Commented on {key}: {browse}"),
    };
    Ok(BuiltinOutput {
        json: json!({ "key": key, "id": id, "created": created, "url": browse }),
        text,
        events: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{Reply, serve};
    use crate::run_builtin;

    const TOKEN: &str = "jira-token-0123456789";

    fn ctx(base: &str) -> BuiltinContext {
        let mut ctx = BuiltinContext::new("/nonexistent");
        ctx.secrets
            .insert("JIRA_BASE_URL".into(), format!("{base}/"));
        ctx.secrets
            .insert("JIRA_EMAIL".into(), "me@example.com".into());
        ctx.secrets.insert("JIRA_API_TOKEN".into(), TOKEN.into());
        ctx.timeout = std::time::Duration::from_secs(5);
        ctx
    }

    fn args(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    fn adf(text: &str) -> Value {
        json!({"type": "doc", "version": 1, "content": [
            {"type": "paragraph", "content": [{"type": "text", "text": text}]}
        ]})
    }

    #[test]
    fn issue_read_sends_basic_auth_and_flattens_the_issue() {
        let comments: Vec<Value> = (1..=12)
            .map(|i| json!({"author": {"displayName": format!("P{i}")}, "created": format!("2024-01-{i:02}"), "body": adf(&format!("comment {i}"))}))
            .collect();
        let issue = json!({
            "key": "PROJ-7",
            "fields": {
                "summary": "Checkout fails",
                "status": {"name": "In Progress"},
                "issuetype": {"name": "Bug"},
                "priority": {"name": "High"},
                "assignee": {"displayName": "Ana"},
                "reporter": {"displayName": "Bo"},
                "labels": ["payments"],
                "updated": "2024-02-01",
                "description": adf(&format!("Leaked {TOKEN} in the logs")),
                "comment": {"total": 12, "comments": comments}
            }
        });
        let (base, seen) = serve(vec![Reply::json(200, issue)]);
        let out = run_builtin("issue.read", &args(json!({"key": "proj-7"})), &ctx(&base)).unwrap();
        let req = seen.recv().unwrap();
        assert_eq!(req.method, "GET");
        assert!(
            req.path.starts_with("/rest/api/3/issue/PROJ-7?fields="),
            "{}",
            req.path
        );
        assert_eq!(
            req.header("authorization"),
            Some(format!("Basic {}", rest::basic_auth("me@example.com", TOKEN)).as_str())
        );
        assert_eq!(out.json["summary"], "Checkout fails");
        assert_eq!(out.json["status"], "In Progress");
        assert_eq!(out.json["assignee"], "Ana");
        assert_eq!(out.json["comment_total"], 12);
        let comments = out.json["comments"].as_array().unwrap();
        assert_eq!(comments.len(), MAX_COMMENTS);
        assert_eq!(comments[9]["body"], "comment 12");
        assert_eq!(out.json["url"], format!("{base}/browse/PROJ-7"));
        assert_eq!(
            out.json["description"],
            "Leaked [REDACTED:JIRA_API_TOKEN] in the logs"
        );
        assert!(
            out.text
                .starts_with("PROJ-7: Checkout fails\nStatus: In Progress"),
            "{}",
            out.text
        );
        assert!(out.text.contains("--- P12, 2024-01-12\ncomment 12"));
        assert!(!out.text.contains(TOKEN) && !out.json.to_string().contains(TOKEN));
    }

    #[test]
    fn issue_comment_posts_adf() {
        let (base, seen) = serve(vec![Reply::json(
            201,
            json!({"id": "10042", "created": "2024-03-01"}),
        )]);
        let mut c = ctx(&base);
        c.approved = true;
        let out = run_builtin(
            "jira.issue.comment",
            &args(json!({"key": "PROJ-7", "body": "Fixed in abc\n\nSee PR"})),
            &c,
        )
        .unwrap();
        let req = seen.recv().unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/rest/api/3/issue/PROJ-7/comment");
        assert!(
            req.header("authorization")
                .is_some_and(|h| h.starts_with("Basic "))
        );
        assert_eq!(req.header("content-type"), Some("application/json"));
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(body, json!({"body": text_to_adf("Fixed in abc\n\nSee PR")}));
        assert_eq!(out.json["id"], "10042");
        assert!(out.text.starts_with("Commented on PROJ-7 (comment 10042)"));
    }

    #[test]
    fn error_statuses_are_reported_without_the_token() {
        let (base, _seen) = serve(vec![
            Reply::json(
                401,
                json!({"errorMessages": [format!("bad token {TOKEN}")]}),
            ),
            Reply::json(404, json!({"errorMessages": ["Issue does not exist"]})),
        ]);
        let c = ctx(&base);
        let err = run_builtin("issue.read", &args(json!({"key": "PROJ-1"})), &c).unwrap_err();
        match &err {
            ConnectorError::Service {
                status, message, ..
            } => {
                assert_eq!(*status, 401);
                assert!(message.contains("[REDACTED:JIRA_API_TOKEN]"), "{message}");
            }
            other => panic!("{other:?}"),
        }
        assert!(!err.to_string().contains(TOKEN));
        let err = run_builtin("issue.read", &args(json!({"key": "PROJ-1"})), &c).unwrap_err();
        assert!(
            err.to_string().contains("404") && err.to_string().contains("Issue does not exist"),
            "{err}"
        );
    }

    #[test]
    fn bad_keys_missing_settings_and_other_hosts_are_refused() {
        let c = ctx("http://127.0.0.1:9");
        for key in ["../admin", "PROJ-1?x=1", "PROJ 1", ""] {
            assert!(
                matches!(
                    run_builtin("issue.read", &args(json!({"key": key})), &c),
                    Err(ConnectorError::BadArgs(_))
                ),
                "{key}"
            );
        }
        let mut c2 = c.clone();
        c2.secrets.remove("JIRA_API_TOKEN");
        assert!(matches!(
            run_builtin("issue.read", &args(json!({"key": "A-1"})), &c2),
            Err(ConnectorError::NotConfigured(_))
        ));
        // A refused connection is a request failure, and the token never shows.
        let err = run_builtin("issue.read", &args(json!({"key": "A-1"})), &c).unwrap_err();
        assert!(matches!(err, ConnectorError::Http(_)), "{err:?}");
        assert!(!err.to_string().contains(TOKEN));
    }
}
