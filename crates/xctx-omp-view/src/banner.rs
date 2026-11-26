//! The banner above the composer: the one problem the user should see now, from the client (omp exited, a line
//! that could not be read) or from the session (a failed turn, a retry, a refused command).

use gpui::{AnyElement, App, ClickEvent, FontWeight, Window, div, prelude::*};
use xctx_omp::{Entry, Level, SessionModel};
use xctx_ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, IconSize, Theme, Tone};

/// How the omp child ended, as the client reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exit {
    /// The exit code, `None` when a signal ended it.
    pub code: Option<i32>,
    /// The end of its error output.
    pub stderr: String,
}

/// What a banner is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerKind {
    /// omp exited: the pane is no longer connected.
    Exited,
    /// omp wrote something the transport could not decode, or omp said a frame did not fit the transport.
    Transport,
    /// The last turn failed (`prompt_result` with an error, or a message that stopped on one).
    TurnFailed,
    /// omp is retrying the turn after a provider error, or switched model to recover.
    Retrying,
    /// A command was refused (a fork, a model switch, a busy session).
    Command,
}

impl BannerKind {
    /// The banner's tone.
    pub fn tone(self) -> Tone {
        match self {
            BannerKind::Exited | BannerKind::TurnFailed => Tone::Danger,
            BannerKind::Transport | BannerKind::Retrying | BannerKind::Command => Tone::Warning,
        }
    }

    fn selector(self) -> &'static str {
        match self {
            BannerKind::Exited => "omp-banner-exited",
            BannerKind::Transport => "omp-banner-transport",
            BannerKind::TurnFailed => "omp-banner-turn-failed",
            BannerKind::Retrying => "omp-banner-retrying",
            BannerKind::Command => "omp-banner-command",
        }
    }
}

/// A problem to show above the composer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    /// What it is about.
    pub kind: BannerKind,
    /// One line.
    pub title: String,
    /// More detail (omp's error output, the undecodable line), if any.
    pub detail: Option<String>,
}

/// How many trailing lines of omp's error output the exit banner keeps.
pub const STDERR_LINES: usize = 4;

/// Every problem to show, most serious first: omp exited, then a transport problem (the client's `malformed`
/// text, the model's `rpc_frame_error`), then the current turn's failure or retry (a notice after the last user
/// message; a retry only while the turn runs), then a refused command. Empty when all is well.
pub fn banners_for(
    model: &SessionModel,
    exit: Option<&Exit>,
    malformed: Option<&str>,
) -> Vec<Banner> {
    let mut out = Vec::new();
    if let Some(exit) = exit {
        let title = match exit.code {
            Some(0) => "omp exited".to_owned(),
            Some(code) => format!("omp exited with code {code}"),
            None => "omp was stopped by a signal".to_owned(),
        };
        let lines: Vec<&str> = exit
            .stderr
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect();
        let tail = lines[lines.len().saturating_sub(STDERR_LINES)..].join("\n");
        out.push(Banner {
            kind: BannerKind::Exited,
            title,
            detail: (!tail.is_empty()).then_some(tail),
        });
    }
    if let Some(line) = malformed {
        out.push(Banner {
            kind: BannerKind::Transport,
            title: "omp sent a line xctx could not read".to_owned(),
            detail: Some(line.to_owned()),
        });
    }
    let refused = model.banner.as_ref().map(|text| Banner {
        kind: if text.contains("transport") {
            BannerKind::Transport
        } else {
            BannerKind::Command
        },
        title: text.clone(),
        detail: None,
    });
    let (transport, command) = match refused {
        Some(b) if b.kind == BannerKind::Transport => (Some(b), None),
        other => (None, other),
    };
    out.extend(transport);
    out.extend(turn_banner(model));
    out.extend(command);
    out
}

/// The first banner of [`banners_for`], if any.
pub fn banner_for(
    model: &SessionModel,
    exit: Option<&Exit>,
    malformed: Option<&str>,
) -> Option<Banner> {
    banners_for(model, exit, malformed).into_iter().next()
}

/// The current turn's failure or retry: the newest such notice (or failed message) after the last user message.
fn turn_banner(model: &SessionModel) -> Option<Banner> {
    let turn_start = model
        .entries
        .iter()
        .rposition(|e| matches!(e, Entry::User { .. }))
        .map_or(0, |i| i + 1);
    model.entries[turn_start..]
        .iter()
        .rev()
        .find_map(|entry| match entry {
            Entry::Notice {
                level: Level::Error,
                text,
            } => Some(Banner {
                kind: BannerKind::TurnFailed,
                title: text.clone(),
                detail: None,
            }),
            Entry::Assistant(a) if !a.streaming => a.error.as_ref().map(|error| Banner {
                kind: BannerKind::TurnFailed,
                title: error.clone(),
                detail: None,
            }),
            Entry::Notice {
                level: Level::Warning,
                text,
            } if model.is_running()
                && (text.starts_with("Retrying") || text.starts_with("Switched to")) =>
            {
                Some(Banner {
                    kind: BannerKind::Retrying,
                    title: text.clone(),
                    detail: None,
                })
            }
            _ => None,
        })
}

type Dismiss = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// Draw `banner` with a Dismiss button. Test selectors: `omp-banner` and `omp-banner-<kind>`.
pub fn render(banner: &Banner, on_dismiss: Dismiss, theme: &Theme) -> AnyElement {
    let tone = banner.kind.tone();
    let colors = theme.tone(tone);
    let kind = banner.kind.selector();
    div()
        .debug_selector(|| "omp-banner".to_owned())
        .flex()
        .items_start()
        .gap(Theme::SPACE_2)
        .mx(Theme::SPACE_3)
        .p(Theme::SPACE_2)
        .rounded(Theme::RADIUS_MD)
        .border_1()
        .border_color(colors.border)
        .bg(colors.bg)
        .child(
            Icon::new(if tone == Tone::Danger {
                IconName::Error
            } else {
                IconName::Warning
            })
            .size(IconSize::Sm)
            .tone(tone),
        )
        .child(
            div()
                .debug_selector(move || kind.to_owned())
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(Theme::SPACE_1)
                .child(
                    div()
                        .text_size(Theme::TEXT_SM)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors.fg)
                        .child(banner.title.clone()),
                )
                .children(banner.detail.clone().map(|detail| {
                    div()
                        .font_family("Menlo")
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.text_muted)
                        .child(detail)
                })),
        )
        .child(
            Button::new("omp-banner-dismiss", "")
                .icon(IconName::Close)
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .tooltip("Dismiss")
                .selector("omp-banner-dismiss")
                .on_click(on_dismiss),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use xctx_omp::Frame;

    use super::*;

    fn apply(model: &mut SessionModel, frame: serde_json::Value) {
        model.apply(Frame::parse(frame));
    }

    fn user(model: &mut SessionModel) {
        apply(
            model,
            json!({"type": "message_start", "message": {"role": "user", "content": "go"}}),
        );
    }

    #[test]
    fn exit_wins_and_keeps_the_end_of_stderr() {
        let model = SessionModel::new();
        let exit = Exit {
            code: Some(2),
            stderr: "a\nb\n\nc\nd\ne\n".into(),
        };
        let b = banner_for(&model, Some(&exit), Some("junk")).expect("banner");
        assert_eq!(b.kind, BannerKind::Exited);
        assert_eq!(b.title, "omp exited with code 2");
        assert_eq!(b.detail.as_deref(), Some("b\nc\nd\ne"));
        let signal = Exit {
            code: None,
            stderr: String::new(),
        };
        assert_eq!(
            banner_for(&model, Some(&signal), None).map(|b| (b.title, b.detail)),
            Some(("omp was stopped by a signal".to_owned(), None))
        );
    }

    #[test]
    fn a_failed_turn_shows_until_the_next_message() {
        let mut model = SessionModel::new();
        user(&mut model);
        apply(
            &mut model,
            json!({"type": "prompt_result", "id": "p1", "status": "error", "error": {"message": "rate limited"}}),
        );
        let b = banner_for(&model, None, None).expect("banner");
        assert_eq!(
            (b.kind, b.title.as_str()),
            (BannerKind::TurnFailed, "rate limited")
        );
        user(&mut model);
        assert_eq!(banner_for(&model, None, None), None);
    }

    #[test]
    fn a_retry_shows_only_while_the_turn_runs() {
        let mut model = SessionModel::new();
        user(&mut model);
        apply(&mut model, json!({"type": "agent_start"}));
        apply(
            &mut model,
            json!({"type": "auto_retry_start", "errorMessage": "overloaded"}),
        );
        let b = banner_for(&model, None, None).expect("banner");
        assert_eq!(b.kind, BannerKind::Retrying);
        assert_eq!(b.title, "Retrying after: overloaded");
        apply(&mut model, json!({"type": "agent_end"}));
        assert_eq!(banner_for(&model, None, None), None);
    }

    #[test]
    fn transport_problems_come_from_the_client_or_the_model() {
        let mut model = SessionModel::new();
        let b = banner_for(&model, None, Some("{oops")).expect("banner");
        assert_eq!(
            (b.kind, b.detail.as_deref()),
            (BannerKind::Transport, Some("{oops"))
        );
        apply(&mut model, json!({"type": "rpc_frame_error"}));
        assert_eq!(
            banner_for(&model, None, None).map(|b| b.kind),
            Some(BannerKind::Transport)
        );
        let mut refused = SessionModel::new();
        apply(
            &mut refused,
            json!({"type": "response", "command": "fork", "success": false, "error": "nothing to fork"}),
        );
        let b = banner_for(&refused, None, None).expect("banner");
        assert_eq!(
            (b.kind, b.title.as_str()),
            (BannerKind::Command, "fork: nothing to fork")
        );
    }
}
