//! Against a real omp. Run by hand: `OMP_BIN=/path/to/omp cargo test -p xctx-omp --test smoke -- --ignored`.
//! It never sends a prompt, so it costs no tokens and needs no login.

use std::time::Duration;

use xctx_omp::{Frame, Incoming, LaunchSpec, OmpClient, SessionModel};

#[test]
#[ignore = "needs a real omp (set OMP_BIN)"]
fn the_handshake_and_first_picture_work_against_real_omp() {
    let omp = std::env::var("OMP_BIN").unwrap_or_else(|_| "omp".to_owned());
    let cwd = tempfile::tempdir().expect("scratch dir");
    let mut spec = LaunchSpec::new(omp, cwd.path());
    spec.args = vec!["--no-session".into(), "--no-title".into()];
    let started = OmpClient::start(&spec).expect("omp starts and says ready");
    assert!(matches!(started.startup.first(), Some(Frame::Ready(_))));

    let mut model = SessionModel::new();
    for f in started.startup {
        model.apply(f);
    }
    while model.commands.is_empty() || model.model.is_none() {
        match started
            .frames
            .recv_timeout(Duration::from_secs(20))
            .expect("omp answers")
        {
            Incoming::Frame(f) => {
                model.apply(f);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
    assert!(model.commands.iter().any(|c| c.name == "session"));
    assert!(model.entries.is_empty());
}
