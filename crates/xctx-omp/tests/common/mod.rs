//! Shared by the replay tests: read a recorded conversation and push what omp said through the model.

#![allow(dead_code)]

use std::path::PathBuf;

use xctx_omp::codec::Decoder;
use xctx_omp::{Effect, Frame, SessionModel};

pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{name}.jsonl"))
}

/// The frames omp said in a fixture, decoded the way the client decodes them.
pub fn frames(name: &str) -> Vec<Frame> {
    let text = std::fs::read_to_string(fixture(name)).expect("fixture");
    let mut decoder = Decoder::default();
    text.lines()
        .filter_map(|l| l.strip_prefix("< "))
        .filter_map(|l| decoder.push_line(l).expect("decodes"))
        .map(Frame::parse)
        .collect()
}

/// Replay every frame; return the model and every effect, in order.
pub fn replay(name: &str) -> (SessionModel, Vec<Effect>) {
    let mut model = SessionModel::new();
    let mut effects = Vec::new();
    for f in frames(name) {
        effects.extend(model.apply(f));
    }
    (model, effects)
}
