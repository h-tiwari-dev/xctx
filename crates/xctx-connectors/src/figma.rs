//! The Figma connector: `frame.read` and `frame.export` through Figma's REST API, with `FIGMA_TOKEN` sent as
//! `X-Figma-Token` to `FIGMA_API_URL` (default [`DEFAULT_FIGMA_API_URL`]).

use std::io::Read;
use std::path::PathBuf;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value, json};
use xctx_core::Redactor;
use xctx_http::Request;

use crate::args::Args;
use crate::error::{ConnectorError, Result};
use crate::rest;
use crate::{BuiltinContext, BuiltinOutput};

/// Figma's API, used when the space sets no `FIGMA_API_URL`.
pub const DEFAULT_FIGMA_API_URL: &str = "https://api.figma.com";

/// Hosts Figma serves rendered images from, which `frame.export` may download from besides the API's own
/// host and [`BuiltinContext::allowed_hosts`]. The token is never sent to them.
pub const FIGMA_IMAGE_HOSTS: &[&str] = &[
    "figma-alpha-api.s3.us-west-2.amazonaws.com",
    "s3-alpha-sig.figma.com",
    "s3-alpha.figma.com",
];

/// The largest image downloaded.
const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

/// The most children and texts `frame.read` lists.
const MAX_CHILDREN: usize = 200;
const MAX_TEXTS: usize = 200;

static FILE_KEY: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Za-z0-9]{1,128}$"));
static NODE_ID: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Za-z0-9:;\-]{1,256}$"));
static DASHED_NODE: LazyLock<Regex> = LazyLock::new(|| re(r"^([0-9]+)-([0-9]+)$"));

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap_or_else(|e| panic!("bad built-in regex {p}: {e}"))
}

struct Figma {
    base: String,
    token: String,
}

fn connect(ctx: &BuiltinContext, masker: &mut Redactor) -> Result<Figma> {
    let token = ctx.setting("FIGMA_TOKEN").ok_or_else(|| {
        ConnectorError::NotConfigured("FIGMA_TOKEN is not set in the space's secrets".to_owned())
    })?;
    masker.extend([("FIGMA_TOKEN".to_owned(), token.to_owned())]);
    let base = rest::base_url(
        ctx.setting("FIGMA_API_URL")
            .unwrap_or(DEFAULT_FIGMA_API_URL),
    )?;
    Ok(Figma {
        base,
        token: token.to_owned(),
    })
}

/// The file key and node id, checked; a node id written with a dash as in Figma's URLs (`1-2`) becomes `1:2`.
fn target(a: &Args<'_>) -> Result<(String, String)> {
    let file = a.required_str("file")?.trim();
    if !FILE_KEY.is_match(file) {
        return Err(ConnectorError::BadArgs(format!(
            "{file:?} is not a Figma file key (the part after /file/ or /design/ in its URL)"
        )));
    }
    let node = a.required_str("node")?.trim();
    if !NODE_ID.is_match(node) {
        return Err(ConnectorError::BadArgs(format!(
            "{node:?} is not a Figma node id such as 1:2"
        )));
    }
    let node = match DASHED_NODE.captures(node) {
        Some(c) => format!("{}:{}", &c[1], &c[2]),
        None => node.to_owned(),
    };
    Ok((file.to_owned(), node))
}

fn encode(id: &str) -> String {
    id.replace(':', "%3A").replace(';', "%3B")
}

fn get(figma: &Figma, url: String, ctx: &BuiltinContext) -> Result<Value> {
    let options = rest::options(&figma.base, ctx)?;
    let request = Request::new("GET", url)
        .header("X-Figma-Token", &figma.token)
        .header("Accept", "application/json");
    rest::send_json("Figma", &request, &options)
}

/// `frame.read`: JSON `{file, file_name, node, name, type, x, y, width, height, children: [{id, name, type}],
/// texts, styles: [{name, type}]}`.
pub(crate) fn frame_read(
    args: &Map<String, Value>,
    ctx: &BuiltinContext,
    masker: &mut Redactor,
) -> Result<BuiltinOutput> {
    let a = Args::new("frame.read", args, &["file", "node"])?;
    let (file, node) = target(&a)?;
    let figma = connect(ctx, masker)?;
    let answer = get(
        &figma,
        format!("{}/v1/files/{file}/nodes?ids={}", figma.base, encode(&node)),
        ctx,
    )?;
    let entry = answer
        .get("nodes")
        .and_then(|n| n.get(&node))
        .filter(|e| !e.is_null());
    let Some(doc) = entry.and_then(|e| e.get("document")) else {
        return Err(ConnectorError::Service {
            service: "Figma".to_owned(),
            status: 404,
            message: format!("node {node} is not in file {file}"),
        });
    };
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    let bbox = doc
        .get("absoluteBoundingBox")
        .cloned()
        .unwrap_or(Value::Null);
    let num = |k: &str| bbox.get(k).and_then(Value::as_f64);
    let kids: &[Value] = doc
        .get("children")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let children: Vec<Value> = kids
        .iter()
        .take(MAX_CHILDREN)
        .map(|c| json!({"id": s(c, "id"), "name": s(c, "name"), "type": s(c, "type")}))
        .collect();
    let mut texts = Vec::new();
    collect_texts(doc, &mut texts);
    let styles: Vec<Value> = entry
        .and_then(|e| e.get("styles"))
        .and_then(Value::as_object)
        .map(|m| {
            m.values()
                .map(|st| json!({"name": s(st, "name"), "type": s(st, "styleType")}))
                .collect()
        })
        .unwrap_or_default();

    let name = s(doc, "name").unwrap_or_default();
    let kind = s(doc, "type").unwrap_or_default();
    let mut text = format!("{name} ({kind}, node {node})\n");
    if let (Some(w), Some(h)) = (num("width"), num("height")) {
        text.push_str(&format!("Size: {w} x {h}\n"));
    }
    if !children.is_empty() {
        text.push_str(&format!("Children ({}):\n", kids.len()));
        for c in &children {
            text.push_str(&format!(
                "- {} ({})\n",
                c["name"].as_str().unwrap_or(""),
                c["type"].as_str().unwrap_or("")
            ));
        }
    }
    if !texts.is_empty() {
        text.push_str("Text:\n");
        for t in &texts {
            text.push_str(&format!("- {t}\n"));
        }
    }
    if !styles.is_empty() {
        text.push_str("Styles:\n");
        for st in &styles {
            text.push_str(&format!(
                "- {} ({})\n",
                st["name"].as_str().unwrap_or(""),
                st["type"].as_str().unwrap_or("")
            ));
        }
    }
    Ok(BuiltinOutput {
        json: json!({
            "file": file,
            "file_name": s(&answer, "name"),
            "node": node,
            "name": name,
            "type": kind,
            "x": num("x"),
            "y": num("y"),
            "width": num("width"),
            "height": num("height"),
            "children": children,
            "child_count": kids.len(),
            "texts": texts,
            "styles": styles,
        }),
        text,
        events: Vec::new(),
    })
}

fn collect_texts(node: &Value, out: &mut Vec<String>) {
    if out.len() >= MAX_TEXTS {
        return;
    }
    if node.get("type").and_then(Value::as_str) == Some("TEXT")
        && let Some(chars) = node.get("characters").and_then(Value::as_str)
        && !chars.trim().is_empty()
    {
        out.push(chars.to_owned());
    }
    if let Some(kids) = node.get("children").and_then(Value::as_array) {
        for k in kids {
            collect_texts(k, out);
        }
    }
}

/// `frame.export`: asks Figma to render the node and returns JSON `{file, node, format, scale, image_url,
/// path}`; `path` is where the image was saved when [`BuiltinContext::scratch_dir`] is set, else null.
pub(crate) fn frame_export(
    args: &Map<String, Value>,
    ctx: &BuiltinContext,
    masker: &mut Redactor,
) -> Result<BuiltinOutput> {
    let a = Args::new("frame.export", args, &["file", "node", "format", "scale"])?;
    let (file, node) = target(&a)?;
    let format = a
        .str("format")?
        .unwrap_or("png")
        .trim()
        .to_ascii_lowercase();
    if !matches!(format.as_str(), "png" | "jpg" | "svg" | "pdf") {
        return Err(ConnectorError::BadArgs(format!(
            "frame.export: format {format:?} is not png, jpg, svg or pdf"
        )));
    }
    let scale = a.int("scale", 1, 4)?.unwrap_or(1);
    let figma = connect(ctx, masker)?;
    let answer = get(
        &figma,
        format!(
            "{}/v1/images/{file}?ids={}&format={format}&scale={scale}",
            figma.base,
            encode(&node)
        ),
        ctx,
    )?;
    if let Some(err) = answer.get("err").and_then(Value::as_str) {
        return Err(ConnectorError::Service {
            service: "Figma".to_owned(),
            status: 200,
            message: err.to_owned(),
        });
    }
    let Some(image_url) = answer
        .get("images")
        .and_then(|i| i.get(&node))
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return Err(ConnectorError::Service {
            service: "Figma".to_owned(),
            status: 200,
            message: format!("no image was rendered for node {node} (is it visible?)"),
        });
    };

    let path = match &ctx.scratch_dir {
        Some(dir) => {
            let name = format!("figma-{file}-{}.{format}", node.replace([':', ';'], "-"));
            let path = dir.join(name);
            download(&image_url, &path, &figma, ctx)?;
            Some(path)
        }
        None => None,
    };
    let mut text = format!("Rendered {node} of {file} as {format} at {scale}x: {image_url}");
    if let Some(p) = &path {
        text.push_str(&format!("\nSaved to {}", p.display()));
    }
    Ok(BuiltinOutput {
        json: json!({
            "file": file,
            "node": node,
            "format": format,
            "scale": scale,
            "image_url": image_url,
            "path": path.as_ref().map(|p| p.display().to_string()),
        }),
        text,
        events: Vec::new(),
    })
}

/// Download `url` (no token sent) into `path`, if its host is the API's, a Figma image host or an extra
/// allowed host.
fn download(url: &str, path: &PathBuf, figma: &Figma, ctx: &BuiltinContext) -> Result<()> {
    let host = xctx_http::host_of(url)
        .map_err(|e| ConnectorError::Refused(format!("image URL {url}: {e}")))?;
    let mut options = rest::options(&figma.base, ctx)?;
    for h in FIGMA_IMAGE_HOSTS {
        options.allow_host(h);
    }
    if !options.is_allowed(&host) {
        return Err(ConnectorError::Refused(format!(
            "the image is on {host}, which is not an allowed host"
        )));
    }
    let https = url::Url::parse(url).is_ok_and(|u| u.scheme() == "https");
    if !https && !rest::is_loopback(&host) {
        return Err(ConnectorError::Refused(format!(
            "the image URL {url} is not https"
        )));
    }
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None)
        .timeout_global(Some(ctx.timeout))
        .tls_config(tls)
        .build()
        .into();
    let response = agent
        .get(url)
        .call()
        .map_err(|e| ConnectorError::Http(format!("downloading the image: {e}")))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(ConnectorError::Service {
            service: "Figma image host".to_owned(),
            status,
            message: "the image could not be downloaded".to_owned(),
        });
    }
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ConnectorError::Http(format!("downloading the image: {e}")))?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(ConnectorError::Refused(format!(
            "the image is larger than {MAX_IMAGE_BYTES} bytes"
        )));
    }
    let io = |e: std::io::Error| ConnectorError::Io(format!("saving {}: {e}", path.display()));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    std::fs::write(path, &bytes).map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{Reply, serve};
    use crate::run_builtin;

    const TOKEN: &str = "figd_secret-token-42";

    fn ctx(base: &str) -> BuiltinContext {
        let mut ctx = BuiltinContext::new("/nonexistent");
        ctx.secrets.insert("FIGMA_TOKEN".into(), TOKEN.into());
        ctx.env.insert("FIGMA_API_URL".into(), base.into());
        ctx.timeout = std::time::Duration::from_secs(5);
        ctx
    }

    fn args(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn frame_read_sends_the_token_and_summarises_the_frame() {
        let nodes = json!({
            "name": "Checkout",
            "nodes": {"1:2": {
                "document": {
                    "id": "1:2", "name": "Cart", "type": "FRAME",
                    "absoluteBoundingBox": {"x": 0.0, "y": 10.0, "width": 375.0, "height": 812.0},
                    "children": [
                        {"id": "1:3", "name": "Title", "type": "TEXT", "characters": "Your cart"},
                        {"id": "1:4", "name": "Pay", "type": "INSTANCE", "children": [
                            {"id": "1:5", "name": "Label", "type": "TEXT", "characters": "Pay now"}
                        ]}
                    ]
                },
                "styles": {"S:1": {"name": "Brand/Primary", "styleType": "FILL"}}
            }}
        });
        let (base, seen) = serve(vec![Reply::json(200, nodes)]);
        let out = run_builtin(
            "frame.read",
            &args(json!({"file": "AbC123", "node": "1-2"})),
            &ctx(&base),
        )
        .unwrap();
        let req = seen.recv().unwrap();
        assert_eq!(req.path, "/v1/files/AbC123/nodes?ids=1%3A2");
        assert_eq!(req.header("x-figma-token"), Some(TOKEN));
        assert_eq!(out.json["name"], "Cart");
        assert_eq!(out.json["type"], "FRAME");
        assert_eq!(out.json["width"], 375.0);
        assert_eq!(out.json["children"][1]["name"], "Pay");
        assert_eq!(out.json["texts"], json!(["Your cart", "Pay now"]));
        assert_eq!(out.json["styles"][0]["name"], "Brand/Primary");
        assert!(
            out.text
                .starts_with("Cart (FRAME, node 1:2)\nSize: 375 x 812\n"),
            "{}",
            out.text
        );
    }

    #[test]
    fn frame_export_asks_for_the_format_and_scale_and_checks_the_image_url() {
        let (base, seen) = serve(vec![Reply::json(
            200,
            json!({"err": null, "images": {"1:2": "not a url"}}),
        )]);
        let dir = tempfile::tempdir().unwrap();
        let mut c = ctx(&base);
        c.scratch_dir = Some(dir.path().join("artifacts"));
        let err = run_builtin(
            "figma.frame.export",
            &args(json!({"file": "AbC123", "node": "1:2", "scale": 2, "format": "SVG"})),
            &c,
        )
        .unwrap_err();
        assert!(matches!(err, ConnectorError::Refused(_)), "{err:?}");
        let req = seen.recv().unwrap();
        assert_eq!(req.path, "/v1/images/AbC123?ids=1%3A2&format=svg&scale=2");
        assert_eq!(req.header("x-figma-token"), Some(TOKEN));
    }

    #[test]
    fn frame_export_downloads_from_an_allowed_host_without_the_token() {
        let png = b"\x89PNG\r\n\x1a\nfake".to_vec();
        let (img_base, img_seen) = serve(vec![Reply {
            status: 200,
            content_type: "image/png",
            body: png.clone(),
        }]);
        let image_url = format!("{img_base}/images/abc.png");
        let (base, seen) = serve(vec![Reply::json(
            200,
            json!({"err": null, "images": {"1:2": image_url}}),
        )]);
        let dir = tempfile::tempdir().unwrap();
        let mut c = ctx(&base);
        let out = run_builtin(
            "frame.export",
            &args(json!({"file": "AbC123", "node": "1:2"})),
            &c,
        )
        .unwrap();
        assert_eq!(out.json["image_url"], image_url);
        assert!(out.json["path"].is_null());
        let _ = seen.recv().unwrap();

        let (base, _seen) = serve(vec![Reply::json(
            200,
            json!({"err": null, "images": {"1:2": image_url}}),
        )]);
        c.env.insert("FIGMA_API_URL".into(), base);
        c.scratch_dir = Some(dir.path().join("artifacts"));
        let out = run_builtin(
            "frame.export",
            &args(json!({"file": "AbC123", "node": "1:2"})),
            &c,
        )
        .unwrap();
        let path = dir.path().join("artifacts/figma-AbC123-1-2.png");
        assert_eq!(out.json["path"], path.display().to_string());
        assert_eq!(std::fs::read(&path).unwrap(), png);
        let img_req = img_seen.recv().unwrap();
        assert_eq!(img_req.path, "/images/abc.png");
        assert!(img_req.header("x-figma-token").is_none());
    }

    #[test]
    fn image_hosts_that_are_not_allowed_are_refused() {
        let (base, _seen) = serve(vec![Reply::json(
            200,
            json!({"err": null, "images": {"1:2": "https://evil.example/x.png"}}),
        )]);
        let dir = tempfile::tempdir().unwrap();
        let mut c = ctx(&base);
        c.scratch_dir = Some(dir.path().to_path_buf());
        let err = run_builtin(
            "frame.export",
            &args(json!({"file": "AbC123", "node": "1:2"})),
            &c,
        )
        .unwrap_err();
        assert!(
            matches!(&err, ConnectorError::Refused(m) if m.contains("evil.example")),
            "{err:?}"
        );
    }

    #[test]
    fn errors_and_bad_arguments() {
        let (base, _seen) = serve(vec![
            Reply::json(
                403,
                json!({"status": 403, "err": format!("Invalid token {TOKEN}")}),
            ),
            Reply::json(200, json!({"name": "F", "nodes": {"9:9": null}})),
        ]);
        let c = ctx(&base);
        let a = args(json!({"file": "AbC123", "node": "9:9"}));
        let err = run_builtin("frame.read", &a, &c).unwrap_err();
        assert!(
            matches!(err, ConnectorError::Service { status: 403, .. }),
            "{err:?}"
        );
        assert!(
            !err.to_string().contains(TOKEN) && err.to_string().contains("[REDACTED:FIGMA_TOKEN]")
        );
        let err = run_builtin("frame.read", &a, &c).unwrap_err();
        assert!(err.to_string().contains("not in file"), "{err}");

        for bad in [
            json!({"file": "../x", "node": "1:2"}),
            json!({"file": "A", "node": "1:2&ids=3"}),
        ] {
            assert!(matches!(
                run_builtin("frame.read", &args(bad), &c),
                Err(ConnectorError::BadArgs(_))
            ));
        }
        assert!(matches!(
            run_builtin(
                "frame.export",
                &args(json!({"file": "A", "node": "1:2", "format": "gif"})),
                &c
            ),
            Err(ConnectorError::BadArgs(_))
        ));
        let mut c2 = c.clone();
        c2.secrets.clear();
        assert!(matches!(
            run_builtin(
                "frame.read",
                &args(json!({"file": "A", "node": "1:2"})),
                &c2
            ),
            Err(ConnectorError::NotConfigured(_))
        ));
    }
}
