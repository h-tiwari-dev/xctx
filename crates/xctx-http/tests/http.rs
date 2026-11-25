//! The API client core against a tiny HTTP server started inside the test on 127.0.0.1. Nothing here touches
//! the network or the user's data.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use xctx_core::{Redactor, SpaceId};
use xctx_http::{
    EventRules, Exchange, FailureKind, HeaderChange, HttpError, HttpEvent, HttpHistory,
    MAX_HISTORY, Outcome, Request, Response, SendOptions, diff, events, http_path, load, replay,
    save, send,
};

const LARGE: usize = 100_000;

struct Server {
    base: String,
    port: u16,
    counter_hits: Arc<AtomicUsize>,
}

impl Server {
    /// The defaults, allowing this server's port on the machine itself.
    fn options(&self) -> SendOptions {
        let mut options = SendOptions::default();
        options.allow_local_port(self.port);
        options
    }
}

/// Start a server answering canned responses by path, one thread per connection.
fn server() -> Server {
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => panic!("cannot bind a test server: {e}"),
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let counter_hits = Arc::new(AtomicUsize::new(0));
    let hits = counter_hits.clone();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let hits = hits.clone();
            thread::spawn(move || handle(stream, &hits));
        }
    });
    Server {
        base: format!("http://127.0.0.1:{port}"),
        port,
        counter_hits,
    }
}

fn handle(stream: TcpStream, hits: &AtomicUsize) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut head = String::new();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
        head.push_str(&line);
    }
    let mut body = vec![0u8; content_length];
    let _ = reader.read_exact(&mut body);
    let body = String::from_utf8_lossy(&body).into_owned();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("").to_owned();

    // `/redirect-port/<status>/<port>` redirects with that status to `/echo` on that port.
    if let Some(rest) = path.strip_prefix("/redirect-port/") {
        let (status, port) = rest.split_once('/').unwrap_or(("302", rest));
        let out = format!(
            "HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\
             location: http://127.0.0.1:{port}/echo\r\n\r\n"
        );
        let mut stream = stream;
        let _ = stream.write_all(out.as_bytes());
        return;
    }
    let (status, headers, payload): (u16, Vec<(&str, String)>, String) = match path.as_str() {
        "/ok" => (
            200,
            vec![("content-type", "text/plain".into())],
            "hello\n".into(),
        ),
        "/counter" => {
            let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
            (
                200,
                vec![("x-count", n.to_string())],
                format!("first\ncount: {n}\nlast\n"),
            )
        }
        "/slow" => {
            thread::sleep(Duration::from_millis(600));
            (200, vec![], "slow\n".into())
        }
        "/error" => (500, vec![], "boom\n".into()),
        "/redirect-away" => (
            302,
            vec![("location", "http://evil.example/steal".into())],
            String::new(),
        ),
        "/redirect-local" => (302, vec![("location", "/ok".into())], String::new()),
        "/redirect-loop" => (
            302,
            vec![("location", "/redirect-loop".into())],
            String::new(),
        ),
        "/large" => (200, vec![], "x".repeat(LARGE)),
        "/echo" => (200, vec![], format!("{method} {path}\n{head}\n{body}")),
        "/secret" => (
            200,
            vec![("set-cookie", "session=abc123; HttpOnly".into())],
            "{\"token\":\"sk_live_supersecret\"}".into(),
        ),
        _ => (404, vec![], "not found\n".into()),
    };
    let mut out = format!(
        "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n",
        payload.len()
    );
    for (name, value) in headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(&payload);
    let mut stream = stream;
    let _ = stream.write_all(out.as_bytes());
    let _ = stream.flush();
}

fn ok<T, E: std::fmt::Debug>(r: Result<T, E>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("unexpected error: {e:?}"),
    }
}

fn response(exchange: &Exchange) -> &Response {
    match &exchange.outcome {
        Outcome::Response(r) => r,
        Outcome::Failed(f) => panic!("expected a response, got {f:?}"),
    }
}

fn get(server: &Server, path: &str) -> Exchange {
    ok(send(
        &Request::new("GET", format!("{}{path}", server.base)),
        &server.options(),
    ))
}

#[test]
fn sends_and_reads_a_response() {
    let s = server();
    let ex = get(&s, "/ok");
    let r = response(&ex);
    assert_eq!(r.status, 200);
    assert_eq!(r.body, "hello\n");
    assert!(!r.truncated);
    assert_eq!(r.header("Content-Type"), Some("text/plain"));
    assert_eq!(ex.id, 0);
}

#[test]
fn sends_headers_and_body() {
    let s = server();
    let req = Request::new("post", format!("{}/echo", s.base))
        .header("X-Test", "yes")
        .body("{\"a\":1}");
    let ex = ok(send(&req, &s.options()));
    let body = &response(&ex).body;
    assert!(body.starts_with("POST /echo\n"), "{body}");
    assert!(body.to_ascii_lowercase().contains("x-test: yes"), "{body}");
    assert!(body.ends_with("{\"a\":1}"), "{body}");
}

#[test]
fn a_500_is_a_response() {
    let s = server();
    let ex = get(&s, "/error");
    assert_eq!(response(&ex).status, 500);
}

#[test]
fn times_out() {
    let s = server();
    let options = SendOptions {
        timeout: Duration::from_millis(150),
        ..s.options()
    };
    let ex = ok(send(
        &Request::new("GET", format!("{}/slow", s.base)),
        &options,
    ));
    let f = match &ex.outcome {
        Outcome::Failed(f) => f,
        Outcome::Response(r) => panic!("expected a timeout, got {r:?}"),
    };
    assert_eq!(f.kind, FailureKind::Timeout, "{f:?}");
    assert!(f.elapsed_ms < 600, "{f:?}");
}

#[test]
fn caps_the_body() {
    let s = server();
    let options = SendOptions {
        max_body_bytes: 1000,
        ..s.options()
    };
    let ex = ok(send(
        &Request::new("GET", format!("{}/large", s.base)),
        &options,
    ));
    let r = response(&ex);
    assert!(r.truncated);
    assert_eq!(r.body.len(), 1000);

    let full = get(&s, "/large");
    assert!(!response(&full).truncated);
    assert_eq!(response(&full).body.len(), LARGE);
}

#[test]
fn follows_a_redirect_to_an_allowed_host() {
    let s = server();
    let ex = get(&s, "/redirect-local");
    assert_eq!(response(&ex).body, "hello\n");
}

#[test]
fn refuses_a_redirect_to_another_host() {
    let s = server();
    let ex = get(&s, "/redirect-away");
    let f = match &ex.outcome {
        Outcome::Failed(f) => f,
        Outcome::Response(r) => panic!("expected a refusal, got {r:?}"),
    };
    assert_eq!(f.kind, FailureKind::RedirectRefused);
    assert!(f.message.contains("evil.example"), "{}", f.message);
}

#[test]
fn stops_a_redirect_loop() {
    let s = server();
    let ex = get(&s, "/redirect-loop");
    assert_eq!(
        ex.failure().map(|f| f.kind),
        Some(FailureKind::TooManyRedirects)
    );
}

#[test]
fn no_redirects_returns_the_redirect() {
    let s = server();
    let options = SendOptions {
        max_redirects: 0,
        ..s.options()
    };
    let ex = ok(send(
        &Request::new("GET", format!("{}/redirect-away", s.base)),
        &options,
    ));
    assert_eq!(response(&ex).status, 302);
}

#[test]
fn refuses_hosts_and_schemes_that_are_not_allowed() {
    let req = Request::new("GET", "http://example.com/");
    match send(&req, &SendOptions::default()) {
        Err(HttpError::HostNotAllowed { host, .. }) => assert_eq!(host, "example.com:80"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    let none = SendOptions {
        allowed_hosts: Vec::new(),
        ..SendOptions::default()
    };
    assert!(matches!(
        send(&Request::new("GET", "http://127.0.0.1:1/"), &none),
        Err(HttpError::HostNotAllowed { .. })
    ));
    assert!(matches!(
        send(
            &Request::new("GET", "ftp://localhost/"),
            &SendOptions::default()
        ),
        Err(HttpError::UnsupportedScheme(_))
    ));
    assert!(matches!(
        send(&Request::new("GET", "not a url"), &SendOptions::default()),
        Err(HttpError::InvalidUrl { .. })
    ));
    let mut local = SendOptions::default();
    local.allow_local_port(80);
    assert!(matches!(
        send(
            &Request::new("GET", "http://localhost/").header("bad name", "x"),
            &local
        ),
        Err(HttpError::InvalidRequest(_))
    ));
}

#[test]
fn allows_configured_base_url_hosts() {
    let options = ok(SendOptions::for_base_urls([
        "https://API.Staging.example:8443/v1",
    ]));
    assert!(options.is_allowed("api.staging.example"));
    assert!(options.allows_url("https://api.staging.example:8443/other"));
    assert!(!options.allows_url("https://api.staging.example/"));
    assert!(!options.allows_url("http://api.staging.example:8443/"));
    assert!(!options.is_allowed("localhost"));
    assert!(!options.is_allowed("staging.example"));
    assert!(SendOptions::for_base_urls(["file:///etc"]).is_err());
}

#[test]
fn a_refused_connection_is_a_failure() {
    // Bind and drop to find a port nothing listens on.
    let port = ok(TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr())).port();
    let mut options = SendOptions::default();
    options.allow_local_port(port);
    let ex = ok(send(
        &Request::new("GET", format!("http://127.0.0.1:{port}/")),
        &options,
    ));
    assert!(ex.failure().is_some(), "{ex:?}");
}

#[test]
fn history_persists_and_is_bounded() {
    let dir = ok(tempfile::tempdir());
    let space = SpaceId::new("demo");
    assert!(ok(load(dir.path(), &space)).exchanges.is_empty());

    let s = server();
    let mut history = HttpHistory::new(space.clone());
    let first = history.record(get(&s, "/ok"));
    let second = history.record(get(&s, "/error"));
    assert_eq!((first, second), (1, 2));
    ok(save(dir.path(), &history));
    let path = ok(http_path(dir.path(), "demo"));
    assert!(path.ends_with("spaces/demo/http.json"));

    let loaded = ok(load(dir.path(), &space));
    assert_eq!(loaded, history);

    let template = history.exchanges[0].clone();
    for _ in 0..(MAX_HISTORY + 10) {
        history.record(template.clone());
    }
    assert_eq!(history.exchanges.len(), MAX_HISTORY);
    assert_eq!(history.exchanges[0].id, 13);
    assert_eq!(history.latest().map(|e| e.id), Some(212));
    ok(save(dir.path(), &history));
    let mut loaded = ok(load(dir.path(), &space));
    assert_eq!(loaded.record(template), 213);
}

#[test]
fn damaged_newer_and_foreign_history_files_are_errors() {
    let dir = ok(tempfile::tempdir());
    let space = SpaceId::new("demo");
    let path = ok(http_path(dir.path(), "demo"));
    ok(std::fs::create_dir_all(path.parent().unwrap_or(dir.path())));

    ok(std::fs::write(&path, "{ not json"));
    assert!(matches!(load(dir.path(), &space), Err(HttpError::Store(_))));
    assert_eq!(ok(std::fs::read_to_string(&path)), "{ not json");

    ok(std::fs::write(&path, r#"{"version":99,"something":"new"}"#));
    assert!(matches!(
        load(dir.path(), &space),
        Err(HttpError::UnsupportedVersion { found: 99, .. })
    ));

    ok(save(dir.path(), &HttpHistory::new(SpaceId::new("other"))));
    let other = ok(http_path(dir.path(), "other"));
    ok(std::fs::copy(other, &path));
    assert!(matches!(
        load(dir.path(), &space),
        Err(HttpError::WrongSpace { .. })
    ));

    assert!(matches!(
        http_path(dir.path(), "../x"),
        Err(HttpError::InvalidSlug(_))
    ));
}

#[test]
fn replays_a_saved_request() {
    let s = server();
    let mut history = HttpHistory::new(SpaceId::new("demo"));
    let id = history.record(ok(send(
        &Request::new("POST", format!("{}/counter", s.base)).body("{}"),
        &s.options(),
    )));
    let runs = ok(replay(&history, id, 11, &s.options()));
    assert_eq!(runs.len(), 11);
    assert_eq!(s.counter_hits.load(Ordering::SeqCst), 12);
    assert!(runs.iter().all(|e| e.request.method == "POST"));
    let ids = history.record_all(runs);
    assert_eq!(ids.first(), Some(&2));
    assert_eq!(ids.last(), Some(&12));

    assert!(matches!(
        replay(&history, 999, 1, &s.options()),
        Err(HttpError::NotFound(999))
    ));
    assert!(matches!(
        replay(&history, id, 0, &s.options()),
        Err(HttpError::BadCount { .. })
    ));
    let none = SendOptions {
        allowed_hosts: Vec::new(),
        ..SendOptions::default()
    };
    assert!(replay(&history, id, 2, &none).is_err());
    assert_eq!(s.counter_hits.load(Ordering::SeqCst), 12);
}

#[test]
fn diffs_two_exchanges() {
    let s = server();
    let mut history = HttpHistory::new(SpaceId::new("demo"));
    let a = history.record(get(&s, "/counter"));
    let b = history.record(get(&s, "/counter"));
    let (a, b) = match (history.get(a), history.get(b)) {
        (Some(a), Some(b)) => (a, b),
        _ => panic!("recorded exchanges missing"),
    };
    let d = diff(a, b);
    assert!(!d.status_changed);
    assert_eq!(
        d.headers,
        vec![HeaderChange::Changed {
            name: "x-count".into(),
            from: "1".into(),
            to: "2".into()
        }]
    );
    assert!(d.body.contains(&"-count: 1".to_owned()), "{:?}", d.body);
    assert!(d.body.contains(&"+count: 2".to_owned()), "{:?}", d.body);
    assert!(d.body.contains(&" first".to_owned()), "{:?}", d.body);
    assert!(d.body[0].starts_with("--- exchange 1"), "{:?}", d.body);

    let same = diff(a, a);
    assert!(same.is_same());

    let err = get(&s, "/error");
    let d = diff(a, &err);
    assert!(d.status_changed);
    assert_eq!((d.a_status, d.b_status), (Some(200), Some(500)));
    assert!(d.headers.contains(&HeaderChange::Removed {
        name: "x-count".into(),
        value: "1".into()
    }));
}

#[test]
fn events_follow_the_thresholds() {
    let s = server();
    let rules = EventRules::default();
    assert_eq!(rules.status_at_least, Some(500));
    assert_eq!(rules.slow_ms, Some(2000));

    let err = get(&s, "/error");
    let evs = events(&err, &rules);
    assert_eq!(evs, vec![HttpEvent::ResponseStatus { status: 500 }]);
    assert_eq!(evs[0].name(), "response.status");
    assert!(events(&get(&s, "/ok"), &rules).is_empty());

    let slow = get(&s, "/slow");
    assert!(events(&slow, &rules).is_empty());
    let quick_rules = EventRules {
        status_at_least: Some(400),
        slow_ms: Some(300),
    };
    let evs = events(&slow, &quick_rules);
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].name(), "response.slow");
    assert!(matches!(evs[0], HttpEvent::ResponseSlow { elapsed_ms } if elapsed_ms >= 300));
    assert_eq!(
        events(&get(&s, "/missing"), &quick_rules),
        vec![HttpEvent::ResponseStatus { status: 404 }]
    );
    let off = EventRules {
        status_at_least: None,
        slow_ms: None,
    };
    assert!(events(&err, &off).is_empty());
}

#[test]
fn masks_auth_headers_and_secrets() {
    let s = server();
    let req = Request::new(
        "GET",
        "http://user:hunter22@127.0.0.1:1/x?key=sk_live_supersecret",
    )
    .header("Authorization", "Bearer abc")
    .header("X-Api-Key", "k")
    .header("Cookie", "a=b")
    .header("X-Trace", "sk_live_supersecret-1");
    let mut ex = get(&s, "/secret");
    ex.request = req;
    let redactor = Redactor::new([("STRIPE_KEY".to_owned(), "sk_live_supersecret".to_owned())]);
    let masked = ex.masked(&redactor);

    let header = |name: &str| {
        masked
            .request
            .headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(
        header("Authorization").as_deref(),
        Some("[REDACTED:Authorization]")
    );
    assert_eq!(header("X-Api-Key").as_deref(), Some("[REDACTED:X-Api-Key]"));
    assert_eq!(header("Cookie").as_deref(), Some("[REDACTED:Cookie]"));
    assert_eq!(
        header("X-Trace").as_deref(),
        Some("[REDACTED:STRIPE_KEY]-1")
    );
    assert!(
        !masked.request.url.contains("hunter22"),
        "{}",
        masked.request.url
    );
    assert!(
        !masked.request.url.contains("sk_live"),
        "{}",
        masked.request.url
    );

    let r = response(&masked);
    assert_eq!(r.header("set-cookie"), Some("[REDACTED:set-cookie]"));
    assert_eq!(r.body, "{\"token\":\"[REDACTED:STRIPE_KEY]\"}");
    // The original is untouched.
    assert!(response(&ex).body.contains("sk_live_supersecret"));
}

#[test]
fn only_the_allowed_port_is_reachable_on_the_machine() {
    let allowed = server();
    let other = server();
    let options = allowed.options();
    let refused = send(&Request::new("GET", format!("{}/ok", other.base)), &options);
    match refused {
        Err(HttpError::HostNotAllowed { host, .. }) => {
            assert_eq!(host, format!("127.0.0.1:{}", other.port));
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(response(&get(&allowed, "/ok")).status, 200);

    // A redirect to the other port is not followed either.
    let ex = get(&allowed, &format!("/redirect-port/302/{}", other.port));
    let f = ex.failure().map(|f| (f.kind, f.message.clone()));
    assert!(
        matches!(&f, Some((FailureKind::RedirectRefused, m)) if m.contains(&other.port.to_string())),
        "{f:?}"
    );
}

#[test]
fn a_redirect_to_another_port_drops_credentials() {
    let first = server();
    let second = server();
    let mut options = first.options();
    options.allow_local_port(second.port);
    let req = Request::new(
        "GET",
        format!("{}/redirect-port/302/{}", first.base, second.port),
    )
    .header("Authorization", "Bearer abc")
    .header("Cookie", "session=1")
    .header("X-Auth-Token", "t")
    .header("X-Test", "kept");
    let ex = ok(send(&req, &options));
    let body = response(&ex).body.to_ascii_lowercase();
    assert!(body.starts_with("get /echo"), "{body}");
    assert!(body.contains("x-test: kept"), "{body}");
    assert!(!body.contains("authorization"), "{body}");
    assert!(!body.contains("cookie"), "{body}");
    assert!(!body.contains("x-auth-token"), "{body}");

    // A same-origin redirect keeps them.
    let same = Request::new(
        "GET",
        format!("{}/redirect-port/307/{}", first.base, first.port),
    )
    .header("Authorization", "Bearer abc");
    let body = response(&ok(send(&same, &options)))
        .body
        .to_ascii_lowercase();
    assert!(body.contains("authorization: bearer abc"), "{body}");
}

#[test]
fn a_307_to_another_origin_does_not_resend_the_body() {
    let first = server();
    let second = server();
    let mut options = first.options();
    options.allow_local_port(second.port);
    for status in [307, 308] {
        let req = Request::new(
            "POST",
            format!("{}/redirect-port/{status}/{}", first.base, second.port),
        )
        .body("{\"card\":\"4242\"}");
        let ex = ok(send(&req, &options));
        assert_eq!(
            ex.failure().map(|f| f.kind),
            Some(FailureKind::RedirectRefused),
            "{ex:?}"
        );
    }
    // Without a body there is nothing to resend.
    let req = Request::new(
        "GET",
        format!("{}/redirect-port/307/{}", first.base, second.port),
    );
    assert_eq!(response(&ok(send(&req, &options))).status, 200);
}

#[test]
fn the_history_file_holds_no_credentials() {
    let dir = ok(tempfile::tempdir());
    let space = SpaceId::new("demo");
    let s = server();
    let mut history = HttpHistory::new(space.clone());
    let secret = history.record(ok(send(
        &Request::new("GET", format!("{}/secret?api_key=k3y-value", s.base))
            .header("Authorization", "Bearer s3cr3t-bearer")
            .header("Cookie", "session=c00kie-value")
            .header("X-Trace", "visible"),
        &s.options(),
    )));
    let plain = history.record(ok(send(
        &Request::new("POST", format!("{}/counter", s.base)).body("{}"),
        &s.options(),
    )));
    ok(save(dir.path(), &history));

    let text = ok(std::fs::read_to_string(ok(http_path(dir.path(), "demo"))));
    for value in ["s3cr3t-bearer", "c00kie-value", "abc123", "k3y-value"] {
        assert!(!text.contains(value), "{value} in {text}");
    }
    assert!(text.contains("visible"), "{text}");
    // Memory keeps the raw values for replay.
    assert!(
        history
            .get(secret)
            .is_some_and(|e| e.request.headers[0].1 == "Bearer s3cr3t-bearer")
    );
    assert_eq!(ok(replay(&history, secret, 1, &s.options())).len(), 1);

    let loaded = ok(load(dir.path(), &space));
    assert_eq!(ok(replay(&loaded, plain, 2, &s.options())).len(), 2);
    match replay(&loaded, secret, 1, &s.options()) {
        Err(e @ HttpError::Masked { .. }) => {
            let message = e.to_string();
            assert!(message.contains("Authorization"), "{message}");
            assert!(message.contains("Cookie"), "{message}");
            assert!(message.contains("not restored"), "{message}");
        }
        other => panic!("expected a masked refusal, got {other:?}"),
    }
}

#[test]
fn a_damaged_history_is_kept_until_quarantined() {
    let dir = ok(tempfile::tempdir());
    let space = SpaceId::new("demo");
    let path = ok(http_path(dir.path(), "demo"));
    ok(std::fs::create_dir_all(path.parent().unwrap_or(dir.path())));
    let history = HttpHistory::new(space.clone());

    ok(std::fs::write(&path, "{ not json"));
    assert!(load(dir.path(), &space).is_err());
    assert!(save(dir.path(), &history).is_err());
    assert_eq!(ok(std::fs::read_to_string(&path)), "{ not json");

    let moved = ok(xctx_http::quarantine(&path));
    assert!(moved.ends_with("http.json.bad"), "{moved:?}");
    assert_eq!(ok(std::fs::read_to_string(&moved)), "{ not json");
    ok(save(dir.path(), &history));
    assert_eq!(ok(load(dir.path(), &space)), history);

    ok(std::fs::write(&path, r#"{"version":99,"something":"new"}"#));
    assert!(matches!(
        save(dir.path(), &history),
        Err(HttpError::UnsupportedVersion { found: 99, .. })
    ));
    let second = ok(xctx_http::quarantine(&path));
    assert!(second.ends_with("http.json.bad.2"), "{second:?}");
    assert!(xctx_http::quarantine(&path).is_err());
}
