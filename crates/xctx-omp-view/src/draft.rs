//! What a composer draft means, without a window: its mode (omp's prefixes), the intent it sends, and the slash
//! and `@file` completions it offers.

use std::ops::Range;

use xctx_omp::Image;
use xctx_omp::model::SlashCommand;

use crate::intent::OmpIntent;

/// The mode a draft puts the composer in, from its prefix, exactly as omp's terminal editor reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftMode {
    /// A message for the agent.
    Prompt,
    /// `!cmd` runs a shell command; `!!cmd` keeps its output out of the model's context.
    Bash {
        /// Whether the draft starts `!!`.
        exclude_from_context: bool,
    },
    /// `$ code` runs Python; `$$ code` keeps it out of the context.
    Python {
        /// Whether the draft starts `$$`.
        exclude_from_context: bool,
    },
    /// `?question` asks a side question.
    Btw,
    /// `/name args` runs a slash command.
    Slash,
}

impl DraftMode {
    /// The mode of `text`. Leading whitespace is ignored, as omp does.
    pub fn of(text: &str) -> DraftMode {
        let t = text.trim_start();
        if let Some(rest) = t.strip_prefix('!') {
            return DraftMode::Bash {
                exclude_from_context: rest.starts_with('!'),
            };
        }
        if let Some(width) = python_prefix(t) {
            return DraftMode::Python {
                exclude_from_context: width == 2,
            };
        }
        if t.starts_with('?') {
            return DraftMode::Btw;
        }
        if t.starts_with('/') {
            return DraftMode::Slash;
        }
        DraftMode::Prompt
    }

    /// The label shown on the composer's border for a mode other than [`DraftMode::Prompt`].
    pub fn label(self) -> Option<&'static str> {
        match self {
            DraftMode::Prompt | DraftMode::Slash => None,
            DraftMode::Bash {
                exclude_from_context: false,
            } => Some("bash"),
            DraftMode::Bash {
                exclude_from_context: true,
            } => Some("bash · not in context"),
            DraftMode::Python {
                exclude_from_context: false,
            } => Some("python"),
            DraftMode::Python {
                exclude_from_context: true,
            } => Some("python · not in context"),
            DraftMode::Btw => Some("btw · side question"),
        }
    }
}

/// `$` or `$$` followed by whitespace: the width of the prefix.
fn python_prefix(t: &str) -> Option<usize> {
    let bytes = t.as_bytes();
    if bytes.first() != Some(&b'$') {
        return None;
    }
    let width = if bytes.get(1) == Some(&b'$') { 2 } else { 1 };
    matches!(bytes.get(width), Some(b' ' | b'\t' | b'\n' | b'\r')).then_some(width)
}

/// Why a draft cannot be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsendable {
    /// Nothing but the prefix (or nothing at all).
    Empty,
    /// omp's RPC mode has no command that runs Python: the `$` prefix works only in omp's terminal UI.
    PythonNeedsTerminal,
}

impl Unsendable {
    /// What to tell the user, or `None` when there is nothing to say (an empty draft).
    pub fn hint(self) -> Option<&'static str> {
        match self {
            Unsendable::Empty => None,
            Unsendable::PythonNeedsTerminal => Some(
                "Python mode runs only in omp's terminal UI: its RPC mode has no Python command.",
            ),
        }
    }
}

/// How a prompt is sent while the agent is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhileRunning {
    /// Interrupt at the next step (Enter).
    Steer,
    /// Wait for the turn to end (Alt+Enter).
    FollowUp,
}

/// The intent `text` sends. `running` says whether a turn is in flight (a message then steers or follows up, as
/// `queue` says); `commands` are omp's slash commands. A message takes the `images` (leaving the vector empty);
/// the other modes leave them where they are.
pub fn intent_for(
    text: &str,
    running: bool,
    queue: WhileRunning,
    commands: &[SlashCommand],
    images: &mut Vec<Image>,
) -> Result<OmpIntent, Unsendable> {
    let t = text.trim();
    match DraftMode::of(t) {
        DraftMode::Bash {
            exclude_from_context,
        } => {
            let command = t.trim_start_matches('!').trim();
            if command.is_empty() {
                return Err(Unsendable::Empty);
            }
            Ok(OmpIntent::Bash {
                command: command.to_owned(),
                exclude_from_context,
            })
        }
        DraftMode::Python { .. } => Err(Unsendable::PythonNeedsTerminal),
        DraftMode::Btw => btw(&t[1..]),
        DraftMode::Slash => {
            let body = &t[1..];
            let (name, args) = body
                .split_once(char::is_whitespace)
                .map_or((body, ""), |(n, a)| (n, a.trim()));
            if name == "btw" {
                return btw(args);
            }
            if commands.iter().any(|c| c.name == name) {
                return Ok(OmpIntent::SlashCommand {
                    name: name.to_owned(),
                    args: args.to_owned(),
                });
            }
            message(t, running, queue, images)
        }
        DraftMode::Prompt => message(t, running, queue, images),
    }
}

fn btw(question: &str) -> Result<OmpIntent, Unsendable> {
    let question = question.trim();
    if question.is_empty() {
        return Err(Unsendable::Empty);
    }
    Ok(OmpIntent::Btw {
        question: question.to_owned(),
    })
}

fn message(
    text: &str,
    running: bool,
    queue: WhileRunning,
    images: &mut Vec<Image>,
) -> Result<OmpIntent, Unsendable> {
    if text.is_empty() && images.is_empty() {
        return Err(Unsendable::Empty);
    }
    let text = text.to_owned();
    let images = std::mem::take(images);
    Ok(match (running, queue) {
        (false, _) => OmpIntent::Prompt { text, images },
        (true, WhileRunning::Steer) => OmpIntent::Steer { text, images },
        (true, WhileRunning::FollowUp) => OmpIntent::FollowUp { text, images },
    })
}

/// The most rows a completion menu shows.
pub const MENU_LIMIT: usize = 8;

/// What the slash menu filters by: the name typed so far when the draft is `/` and a name with no space yet.
pub fn slash_query(text: &str) -> Option<&str> {
    let rest = text.strip_prefix('/')?;
    (!rest.contains(char::is_whitespace)).then_some(rest)
}

/// The commands matching `query`: names starting with it first, then names containing it, each in omp's order,
/// at most [`MENU_LIMIT`].
pub fn slash_matches<'a>(commands: &'a [SlashCommand], query: &str) -> Vec<&'a SlashCommand> {
    let query = query.to_lowercase();
    let starts = commands
        .iter()
        .filter(|c| c.name.to_lowercase().starts_with(&query));
    let contains = commands.iter().filter(|c| {
        let name = c.name.to_lowercase();
        !name.starts_with(&query) && name.contains(&query)
    });
    starts.chain(contains).take(MENU_LIMIT).collect()
}

/// The `@file` token the caret is in: its byte range (from the `@` to the caret) and the text after the `@`. The
/// `@` must start the draft or follow whitespace, and nothing between it and the caret is whitespace.
pub fn file_query(text: &str, cursor: usize) -> Option<(Range<usize>, &str)> {
    let before = text.get(..cursor)?;
    let at = before.rfind('@')?;
    let token = &before[at + 1..];
    if token.contains(char::is_whitespace) {
        return None;
    }
    let starts_word = before[..at]
        .chars()
        .next_back()
        .is_none_or(char::is_whitespace);
    starts_word.then_some((at..cursor, token))
}

/// `text` with the token at `range` replaced by `@path ` (a trailing space ends the token).
pub fn complete_file(text: &str, range: Range<usize>, path: &str) -> String {
    let mut out = String::with_capacity(text.len() + path.len() + 2);
    out.push_str(&text[..range.start]);
    out.push('@');
    out.push_str(path);
    out.push(' ');
    out.push_str(&text[range.end..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(name: &str) -> SlashCommand {
        SlashCommand {
            name: name.to_owned(),
            description: String::new(),
            hint: None,
        }
    }

    #[test]
    fn prefixes_pick_the_mode_as_omp_does() {
        assert_eq!(DraftMode::of("fix it"), DraftMode::Prompt);
        assert_eq!(
            DraftMode::of("  !ls"),
            DraftMode::Bash {
                exclude_from_context: false
            }
        );
        assert_eq!(
            DraftMode::of("!!ls"),
            DraftMode::Bash {
                exclude_from_context: true
            }
        );
        assert_eq!(
            DraftMode::of("$ print(1)"),
            DraftMode::Python {
                exclude_from_context: false
            }
        );
        assert_eq!(
            DraftMode::of("$$\tx"),
            DraftMode::Python {
                exclude_from_context: true
            }
        );
        assert_eq!(DraftMode::of("$HOME is set"), DraftMode::Prompt);
        assert_eq!(DraftMode::of("?why"), DraftMode::Btw);
        assert_eq!(DraftMode::of("/jobs"), DraftMode::Slash);
    }

    #[test]
    fn a_draft_sends_the_intent_its_mode_and_the_turn_call_for() {
        let commands = [command("jobs"), command("compact")];
        let send =
            |text: &str, running, queue| intent_for(text, running, queue, &commands, &mut vec![]);
        assert_eq!(
            send("hi", false, WhileRunning::Steer),
            Ok(OmpIntent::Prompt {
                text: "hi".into(),
                images: vec![]
            })
        );
        assert_eq!(
            send("hi", true, WhileRunning::Steer),
            Ok(OmpIntent::Steer {
                text: "hi".into(),
                images: vec![]
            })
        );
        assert_eq!(
            send("hi", true, WhileRunning::FollowUp),
            Ok(OmpIntent::FollowUp {
                text: "hi".into(),
                images: vec![]
            })
        );
        assert_eq!(
            send("!! git status ", false, WhileRunning::Steer),
            Ok(OmpIntent::Bash {
                command: "git status".into(),
                exclude_from_context: true
            })
        );
        assert_eq!(
            send("? what is x", true, WhileRunning::Steer),
            Ok(OmpIntent::Btw {
                question: "what is x".into()
            })
        );
        assert_eq!(
            send("/btw what is x", false, WhileRunning::Steer),
            Ok(OmpIntent::Btw {
                question: "what is x".into()
            })
        );
        assert_eq!(
            send("/compact keep tests", false, WhileRunning::Steer),
            Ok(OmpIntent::SlashCommand {
                name: "compact".into(),
                args: "keep tests".into()
            })
        );
        assert!(matches!(
            send("/not-a-command", false, WhileRunning::Steer),
            Ok(OmpIntent::Prompt { .. })
        ));
        assert_eq!(
            send("$ 1+1", false, WhileRunning::Steer),
            Err(Unsendable::PythonNeedsTerminal)
        );
        assert_eq!(
            send("!", false, WhileRunning::Steer),
            Err(Unsendable::Empty)
        );
        assert_eq!(
            send("  ", false, WhileRunning::Steer),
            Err(Unsendable::Empty)
        );
    }

    #[test]
    fn a_message_takes_the_images_and_other_modes_leave_them() {
        let image = Image {
            mime_type: "image/png".into(),
            data: "AA".into(),
        };
        let mut images = vec![image];
        let bash = intent_for("!ls", false, WhileRunning::Steer, &[], &mut images);
        assert!(matches!(bash, Ok(OmpIntent::Bash { .. })));
        assert_eq!(images.len(), 1);
        let intent = intent_for("", false, WhileRunning::Steer, &[], &mut images);
        assert!(matches!(intent, Ok(OmpIntent::Prompt { ref images, .. }) if images.len() == 1));
        assert!(images.is_empty());
    }

    #[test]
    fn the_slash_menu_filters_by_prefix_then_substring() {
        let commands = [
            command("compact"),
            command("context"),
            command("jobs"),
            command("recompact"),
        ];
        assert_eq!(slash_query("/co"), Some("co"));
        assert_eq!(slash_query("/compact now"), None);
        assert_eq!(slash_query("hi"), None);
        let names: Vec<&str> = slash_matches(&commands, "co")
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, ["compact", "context", "recompact"]);
        assert_eq!(slash_matches(&commands, "").len(), 4);
    }

    #[test]
    fn the_file_token_is_the_at_word_under_the_caret() {
        let text = "look at @src/ma";
        let (range, query) = file_query(text, text.len()).expect("a token");
        assert_eq!((range.clone(), query), (8..text.len(), "src/ma"));
        assert_eq!(
            complete_file(text, range, "src/main.rs"),
            "look at @src/main.rs "
        );
        assert_eq!(file_query("mail me@host", 12), None);
        assert_eq!(file_query("@a b", 4), None);
        assert_eq!(file_query("@", 1).map(|(_, q)| q), Some(""));
    }
}
