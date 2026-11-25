//! Drives a real Chrome against a page served from this test. Skips (and passes) when no Chrome is
//! installed, so CI on a machine without one stays green.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use xctx_browser::{
    Browser, BrowserError, LaunchOptions, ScreenshotOptions, UrlPolicy, find_chrome,
};

const PAGE: &str = r#"<!doctype html>
<html><head><title>xctx test</title></head>
<body style="height: 3000px"><h1>hello</h1>
<script>console.log("hello from the page", 42);</script>
<script>fetch("/missing").catch(() => {});</script>
<script>throw new Error("boom from the page");</script>
</body></html>"#;

/// Serves `PAGE` at `/`, a 204 for the favicon and a 404 for everything else, forever, on a thread.
fn serve() -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || answer(stream));
        }
    });
    Ok(port)
}

fn answer(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let path = head.split_whitespace().nth(1).unwrap_or("/");
    let (status, body) = match path {
        "/" => ("200 OK", PAGE),
        "/favicon.ico" => ("204 No Content", ""),
        _ => ("404 Not Found", "not here"),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
}

fn process_exists(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks whether the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Whether any process still has `profile` on its command line (Chrome's helpers all do).
fn processes_using(profile: &Path) -> bool {
    std::process::Command::new("pgrep")
        .arg("-f")
        .arg(profile)
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

fn wait_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    done()
}

#[test]
fn a_local_page_is_opened_captured_and_closed() -> Result<(), Box<dyn std::error::Error>> {
    let chrome = match find_chrome() {
        Ok(chrome) => chrome,
        Err(e) => {
            eprintln!("skipping the Chrome test: {e}");
            return Ok(());
        }
    };
    let started = Instant::now();
    let site = serve()?;
    let other = serve()?;
    let profile = tempfile::tempdir()?;
    let profile_dir = profile.path().join("profile");
    let mut options = LaunchOptions::new(chrome, &profile_dir);
    options.headless = true;
    options.window_size = Some((800, 600));
    let mut browser = Browser::launch(options)?;
    let pid = browser.pid().ok_or("a launched browser has a pid")?;
    assert!(process_exists(pid));
    assert!(profile_dir.join("DevToolsActivePort").exists());
    assert!(!browser.version()?.browser.is_empty());

    // An attached client sees the same browser and leaves it running when dropped.
    let attached = Browser::attach(browser.port())?;
    assert_eq!(attached.pid(), None);
    drop(attached);
    assert!(process_exists(pid));

    let mut policy = UrlPolicy::default();
    policy.allow_local_port(site);
    policy.allow_local_port(1);
    let mut page = browser.new_page(policy)?;
    assert!(
        browser
            .targets()?
            .iter()
            .any(|t| t.id == page.target_id() && t.kind == "page")
    );

    let url = format!("http://127.0.0.1:{site}/");
    page.open(&url)?;
    assert_eq!(page.url()?, url);
    assert_eq!(page.current_url()?, url);
    assert_eq!(page.allowed_current_url()?, url);
    assert_eq!(page.evaluate("document.title")?, "xctx test");
    assert_eq!(page.evaluate("Promise.resolve(1 + 2)")?, 3);
    assert!(matches!(
        page.evaluate("(() => { throw new Error('nope') })()"),
        Err(BrowserError::Script(text)) if text.contains("nope")
    ));

    let shot = page.screenshot()?;
    assert!(is_png(&shot), "the screenshot is a PNG");
    let full = page.screenshot_with(ScreenshotOptions {
        full_page: true,
        ..ScreenshotOptions::default()
    })?;
    assert!(is_png(&full) && full.len() > shot.len() / 2);

    let collected = |page: &mut xctx_browser::Page| -> xctx_browser::Result<bool> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let console = page.console()?;
            let network = page.network_errors()?;
            let logged = console
                .iter()
                .any(|e| e.level == "log" && e.text == "hello from the page 42");
            let threw = console
                .iter()
                .any(|e| e.is_exception() && e.text.contains("boom from the page"));
            let missing = network
                .iter()
                .any(|n| n.status == Some(404) && n.url.ends_with("/missing"));
            if logged && threw && missing {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                eprintln!("console: {console:?}\nnetwork: {network:?}");
                return Ok(false);
            }
            page.pump(Duration::from_millis(100))?;
        }
    };
    assert!(
        collected(&mut page)?,
        "the console and the 404 are collected"
    );

    page.reload()?;
    assert!(collected(&mut page)?, "a reload collects them again");

    assert!(matches!(
        page.open("https://example.com/"),
        Err(BrowserError::HostNotAllowed { host, .. }) if host == "example.com:443"
    ));
    assert!(matches!(
        page.open(&format!("http://127.0.0.1:{other}/")),
        Err(BrowserError::HostNotAllowed { .. })
    ));
    assert!(matches!(
        page.open("http://127.0.0.1:1/"),
        Err(BrowserError::Navigation { .. })
    ));
    let failed = page.open("http://127.0.0.1:1/");
    assert!(
        failed.as_ref().is_err_and(|e| !e.is_transport()),
        "{failed:?}"
    );

    // The page itself leaves for a port the policy does not allow: its URL is no longer allowed.
    page.open(&url)?;
    page.evaluate(&format!("location.href = 'http://127.0.0.1:{other}/'; 1"))?;
    assert!(
        wait_until(Duration::from_secs(5), || page
            .current_url()
            .is_ok_and(|u| u.contains(&other.to_string()))),
        "the page navigated away"
    );
    assert!(matches!(
        page.allowed_current_url(),
        Err(BrowserError::HostNotAllowed { .. })
    ));
    // A late load from that navigation does not end the next open's wait early, and the page is back.
    page.open(&url)?;
    assert_eq!(page.allowed_current_url()?, url);
    assert_eq!(page.evaluate("document.title")?, "xctx test");

    let id = page.target_id().to_owned();
    page.close()?;
    assert!(wait_until(Duration::from_secs(5), || {
        browser
            .targets()
            .is_ok_and(|targets| targets.iter().all(|t| t.id != id))
    }));

    drop(browser);
    assert!(!process_exists(pid), "Chrome is gone after drop");
    assert!(
        wait_until(Duration::from_secs(5), || !processes_using(&profile_dir)),
        "no Chrome helper is left behind"
    );
    eprintln!("the Chrome test took {:?}", started.elapsed());
    Ok(())
}

#[test]
fn an_orphaned_chrome_on_the_profile_does_not_block_the_next_launch()
-> Result<(), Box<dyn std::error::Error>> {
    let chrome = match find_chrome() {
        Ok(chrome) => chrome,
        Err(e) => {
            eprintln!("skipping the Chrome test: {e}");
            return Ok(());
        }
    };
    let profile = tempfile::tempdir()?;
    let profile_dir = profile.path().join("profile");
    let mut options = LaunchOptions::new(chrome, &profile_dir);
    options.headless = true;
    let orphan = Browser::launch(options.clone())?;
    let orphan_pid = orphan.pid().ok_or("a launched browser has a pid")?;
    // As if xctx died: nothing quits this Chrome.
    std::mem::forget(orphan);
    assert!(process_exists(orphan_pid));
    // Should the test fail, do not leave the orphan running.
    struct KillGroup(u32);
    impl Drop for KillGroup {
        fn drop(&mut self) {
            if let Ok(pid) = libc::pid_t::try_from(self.0) {
                // SAFETY: only signals the group of the Chrome this test started.
                unsafe {
                    libc::killpg(pid, libc::SIGKILL);
                }
            }
        }
    }
    let _cleanup = KillGroup(orphan_pid);

    let browser = Browser::launch(options)?;
    assert!(!browser.version()?.browser.is_empty());
    assert_ne!(browser.pid(), Some(orphan_pid));
    // The orphan is our child here (the test forgot it), so reap it ourselves to see it is gone.
    let reaped = wait_until(Duration::from_secs(5), || {
        let Ok(pid) = libc::pid_t::try_from(orphan_pid) else {
            return true;
        };
        let mut status = 0;
        // SAFETY: waitpid on our own child only writes the status.
        unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) == pid }
    });
    assert!(reaped, "the orphaned Chrome quit");
    drop(browser);
    assert!(
        wait_until(Duration::from_secs(5), || !processes_using(&profile_dir)),
        "no Chrome helper is left behind"
    );
    Ok(())
}

#[test]
fn a_chrome_that_is_not_there_fails_to_launch() {
    let profile = tempfile::tempdir().ok();
    let dir = profile
        .as_ref()
        .map(|p| p.path().join("profile"))
        .unwrap_or_default();
    let options = LaunchOptions::new("/nonexistent/Google Chrome", dir);
    assert!(matches!(
        Browser::launch(options),
        Err(BrowserError::Spawn { .. })
    ));
}
