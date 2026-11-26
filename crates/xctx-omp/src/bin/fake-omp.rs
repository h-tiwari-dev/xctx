//! A stand-in for `omp --mode rpc-ui` that replays a recorded conversation.
//!
//! `fake-omp --fixture FILE` reads a fixture (`< frame` lines omp said, `> command` lines the client said). It
//! emits the frames up to the first command, then for each recorded command waits for the same kind of command
//! on its input, swaps the recorded command id for the real one in the frames that follow, and emits them.
//! `FAKE_OMP_CHUNK=N` re-sends every frame longer than N bytes as `rpc_chunk` pieces once the client has
//! negotiated protocol v2. When the script ends it waits for its input to close, then exits.

use std::io::{BufRead, Write};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

struct Step {
    expect: Option<Value>,
    emit: Vec<String>,
}

fn load(path: &str) -> Vec<Step> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("fake-omp: cannot read {path}: {e}");
        std::process::exit(2);
    });
    let mut steps = vec![Step {
        expect: None,
        emit: Vec::new(),
    }];
    for line in text.lines() {
        if let Some(frame) = line.strip_prefix("< ") {
            steps
                .last_mut()
                .expect("a step")
                .emit
                .push(frame.to_owned());
        } else if let Some(command) = line.strip_prefix("> ") {
            steps.push(Step {
                expect: serde_json::from_str(command).ok(),
                emit: Vec::new(),
            });
        }
    }
    // The recorder wrote its first command before omp's `ready` arrived; omp always says `ready` first.
    let ready = steps.iter_mut().find_map(|s| {
        s.emit
            .iter()
            .position(|f| f.starts_with("{\"type\":\"ready\""))
            .map(|i| s.emit.remove(i))
    });
    if let Some(ready) = ready {
        steps[0].emit.insert(0, ready);
    }
    steps
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut fixture = None;
    while let Some(a) = args.next() {
        if a == "--fixture" {
            fixture = args.next();
        }
    }
    let Some(fixture) = fixture else {
        eprintln!("fake-omp: --fixture FILE is required");
        std::process::exit(2);
    };
    let chunk: Option<usize> = std::env::var("FAKE_OMP_CHUNK")
        .ok()
        .and_then(|v| v.parse().ok());
    let steps = load(&fixture);

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut negotiated = false;
    let mut counter = 0u64;

    for step in steps {
        let mut real_id: Option<String> = None;
        if let Some(expect) = &step.expect {
            let want = expect.get("type").and_then(Value::as_str).unwrap_or("");
            loop {
                let mut line = String::new();
                if input.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let Ok(got) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if got.get("type").and_then(Value::as_str) == Some(want) {
                    real_id = got.get("id").and_then(Value::as_str).map(str::to_owned);
                    if want == "negotiate_protocol" {
                        negotiated = true;
                    }
                    break;
                }
            }
        }
        let recorded_id = step
            .expect
            .as_ref()
            .and_then(|e| e.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        for frame in &step.emit {
            let mut frame = frame.clone();
            if let (Some(old), Some(new)) = (&recorded_id, &real_id) {
                frame = frame.replace(&format!("\"id\":\"{old}\""), &format!("\"id\":\"{new}\""));
            }
            match chunk.filter(|n| negotiated && frame.len() > *n) {
                Some(n) => {
                    counter += 1;
                    for piece in chunks(&frame, n, counter) {
                        let _ = writeln!(out, "{piece}");
                    }
                }
                None => {
                    let _ = writeln!(out, "{frame}");
                }
            }
            let _ = out.flush();
        }
    }
    let mut sink = String::new();
    while input.read_line(&mut sink).unwrap_or(0) > 0 {
        sink.clear();
    }
}

/// `frame` as `rpc_chunk` lines of at most `size` raw bytes each.
fn chunks(frame: &str, size: usize, n: u64) -> Vec<String> {
    let bytes = frame.as_bytes();
    let count = bytes.len().div_ceil(size);
    bytes
        .chunks(size)
        .enumerate()
        .map(|(index, part)| {
            json!({
                "type": "rpc_chunk",
                "chunkId": format!("fake-{n}"),
                "index": index,
                "count": count,
                "byteLength": bytes.len(),
                "data": STANDARD.encode(part),
            })
            .to_string()
        })
        .collect()
}
