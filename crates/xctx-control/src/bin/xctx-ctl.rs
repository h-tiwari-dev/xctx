//! `xctx-ctl`: drive a running xctx window from a shell or an agent. See `xctx-ctl help`.

use std::process::ExitCode;

use xctx_control::cli::{self, AGENT_ENV, ATTEMPT_ENV, Command, Defaults, SPACE_ENV};
use xctx_control::{Client, ControlError};

fn main() -> ExitCode {
    let defaults = Defaults {
        space: std::env::var(SPACE_ENV).ok(),
        agent: std::env::var(AGENT_ENV).ok(),
        attempt: std::env::var(ATTEMPT_ENV).ok(),
    };
    let command = match cli::parse_args_with(std::env::args().skip(1), &defaults) {
        Ok(command) => command,
        Err(e) => {
            eprintln!("xctx-ctl: {e}");
            return ExitCode::from(2);
        }
    };
    let (socket, compact, call) = match command {
        Command::Help => {
            print!("{}", cli::help());
            return ExitCode::SUCCESS;
        }
        Command::Version => {
            println!("xctx-ctl {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Command::Call {
            socket,
            compact,
            call,
        } => (socket, compact, call),
    };
    let client = match socket {
        Some(path) => Client::connect(path),
        None => Client::connect_default(),
    };
    let result = client.and_then(|mut c| c.call(&call));
    match result {
        Ok(value) => {
            let text = if compact {
                serde_json::to_string(&value)
            } else {
                serde_json::to_string_pretty(&value)
            };
            match text {
                Ok(text) => {
                    println!("{text}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("xctx-ctl: {e}");
                    ExitCode::from(1)
                }
            }
        }
        Err(ControlError::Rpc(e)) => {
            eprintln!("xctx-ctl: {}: {}", call.method(), e.message);
            ExitCode::from(1)
        }
        Err(e @ (ControlError::NotRunning(_) | ControlError::NoSocketPath(_))) => {
            eprintln!("xctx-ctl: {e}");
            ExitCode::from(3)
        }
        Err(e) => {
            eprintln!("xctx-ctl: {e}");
            ExitCode::from(1)
        }
    }
}
