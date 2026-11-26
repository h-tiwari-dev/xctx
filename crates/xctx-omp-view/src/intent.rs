//! What the user asked the pane to do, for the host to send to omp.

use xctx_omp::{Command, Image, Queue, Streaming};

/// One thing the user did in an [`crate::OmpView`] that omp has to hear about. The view never talks to omp
/// itself: the host subscribes to these and sends [`OmpIntent::command`] through its `OmpClient`.
#[derive(Debug, Clone, PartialEq)]
pub enum OmpIntent {
    /// Start a turn with a message (the agent is idle).
    Prompt {
        /// The message.
        text: String,
        /// Pasted or dropped images.
        images: Vec<Image>,
    },
    /// Interrupt the running turn with a message, delivered at the next step boundary.
    Steer {
        /// The message.
        text: String,
        /// Pasted or dropped images.
        images: Vec<Image>,
    },
    /// Queue a message for when the running turn would otherwise stop.
    FollowUp {
        /// The message.
        text: String,
        /// Pasted or dropped images.
        images: Vec<Image>,
    },
    /// Stop the running turn (the Stop button, or Esc while the agent works).
    Abort,
    /// Drop a queued message (its chip's remove button).
    RemoveQueued {
        /// The chip's text, as omp reported it.
        message: String,
        /// The queue it sits in.
        queue: Queue,
    },
    /// Move a queued follow-up to the steering queue.
    PromoteQueued {
        /// The chip's text.
        message: String,
    },
    /// Take a queued message back to edit it: omp drops it from the queue, and the view has already put its text
    /// in the composer to be sent again.
    EditQueued {
        /// The chip's text.
        message: String,
        /// The queue it sat in.
        queue: Queue,
    },
    /// Ask omp for its model picker rows, to fill the model menu ([`crate::ModelChoice::from_picker`]).
    ListModels,
    /// Switch the session's model.
    SetModel {
        /// The provider, such as `anthropic`.
        provider: String,
        /// The model id.
        model_id: String,
    },
    /// Set the thinking level (effort), one of the session's `thinking_levels`.
    SetThinkingLevel {
        /// The level, such as `high`.
        level: String,
    },
    /// Run one of omp's slash commands.
    SlashCommand {
        /// The command name, without the slash.
        name: String,
        /// What followed the name, trimmed; empty when nothing did.
        args: String,
    },
    /// Run a shell command the way omp's `!` prefix does.
    Bash {
        /// The command line.
        command: String,
        /// omp's `!!`: the output stays out of the model's context.
        exclude_from_context: bool,
    },
    /// Ask a side question that does not enter the conversation (omp's `?` prefix).
    Btw {
        /// The question.
        question: String,
    },
}

/// How many model picker rows [`OmpIntent::ListModels`] asks for.
pub const MODEL_PICKER_LIMIT: u32 = 50;

impl OmpIntent {
    /// The command that carries this intent to omp.
    pub fn command(&self) -> Command {
        match self {
            OmpIntent::Prompt { text, images } => Command::prompt(text, images, None),
            OmpIntent::Steer { text, images } if images.is_empty() => Command::steer(text),
            OmpIntent::Steer { text, images } => {
                Command::prompt(text, images, Some(Streaming::Steer))
            }
            OmpIntent::FollowUp { text, images } if images.is_empty() => Command::follow_up(text),
            OmpIntent::FollowUp { text, images } => {
                Command::prompt(text, images, Some(Streaming::FollowUp))
            }
            OmpIntent::Abort => Command::abort(),
            OmpIntent::RemoveQueued { message, queue }
            | OmpIntent::EditQueued { message, queue } => {
                Command::remove_queued_message(message, *queue)
            }
            OmpIntent::PromoteQueued { message } => Command::promote_queued_message(message),
            OmpIntent::ListModels => Command::get_model_picker("", MODEL_PICKER_LIMIT),
            OmpIntent::SetModel { provider, model_id } => Command::set_model(provider, model_id),
            OmpIntent::SetThinkingLevel { level } => Command::set_thinking_level(level),
            OmpIntent::SlashCommand { name, args } if args.is_empty() => {
                Command::prompt(&format!("/{name}"), &[], None)
            }
            OmpIntent::SlashCommand { name, args } => {
                Command::prompt(&format!("/{name} {args}"), &[], None)
            }
            OmpIntent::Bash {
                command,
                exclude_from_context,
            } => Command::bash(command, *exclude_from_context),
            OmpIntent::Btw { question } => Command::btw(question),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn wire(intent: OmpIntent) -> serde_json::Value {
        intent.command().as_value().clone()
    }

    #[test]
    fn messages_use_the_queue_commands_and_fall_back_to_prompt_when_they_carry_images() {
        let image = Image {
            mime_type: "image/png".into(),
            data: "AAAA".into(),
        };
        assert_eq!(
            wire(OmpIntent::Steer {
                text: "stop at 5".into(),
                images: vec![]
            }),
            json!({"type": "steer", "message": "stop at 5"})
        );
        let steer = wire(OmpIntent::Steer {
            text: "look".into(),
            images: vec![image.clone()],
        });
        assert_eq!(steer["type"], "prompt");
        assert_eq!(steer["streamingBehavior"], "steer");
        assert_eq!(steer["images"][0]["mimeType"], "image/png");
        assert_eq!(
            wire(OmpIntent::FollowUp {
                text: "then DONE".into(),
                images: vec![]
            }),
            json!({"type": "follow_up", "message": "then DONE"})
        );
        assert_eq!(
            wire(OmpIntent::Prompt {
                text: "hi".into(),
                images: vec![]
            }),
            json!({"type": "prompt", "message": "hi"})
        );
    }

    #[test]
    fn queue_edits_remove_the_message_from_its_queue() {
        assert_eq!(
            wire(OmpIntent::EditQueued {
                message: "x".into(),
                queue: Queue::FollowUp
            }),
            json!({"type": "remove_queued_message", "message": "x", "queue": "followUp"})
        );
        assert_eq!(
            wire(OmpIntent::PromoteQueued {
                message: "x".into()
            }),
            json!({"type": "promote_queued_message", "message": "x"})
        );
    }

    #[test]
    fn modes_and_commands_map_to_their_rpc_commands() {
        assert_eq!(
            wire(OmpIntent::SlashCommand {
                name: "jobs".into(),
                args: String::new()
            }),
            json!({"type": "prompt", "message": "/jobs"})
        );
        assert_eq!(
            wire(OmpIntent::SlashCommand {
                name: "compact".into(),
                args: "keep tests".into()
            })["message"],
            "/compact keep tests"
        );
        assert_eq!(
            wire(OmpIntent::Bash {
                command: "ls".into(),
                exclude_from_context: true
            }),
            json!({"type": "bash", "command": "ls", "excludeFromContext": true})
        );
        assert_eq!(
            wire(OmpIntent::Btw {
                question: "why?".into()
            }),
            json!({"type": "btw", "question": "why?"})
        );
        assert_eq!(wire(OmpIntent::Abort), json!({"type": "abort"}));
        assert_eq!(
            wire(OmpIntent::SetThinkingLevel {
                level: "high".into()
            }),
            json!({"type": "set_thinking_level", "level": "high"})
        );
        assert_eq!(wire(OmpIntent::ListModels)["type"], "get_model_picker");
    }
}
