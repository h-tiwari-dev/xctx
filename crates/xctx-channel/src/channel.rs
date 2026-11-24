//! The space Channel: messages between a space's agents, addressed to one agent or broadcast to the space.
//!
//! The history is bounded by [`MAX_MESSAGES`]; posting beyond it drops the oldest messages, so an agent posting in
//! a loop cannot grow memory or `shared.json` without limit.

use serde::{Deserialize, Serialize};
use xctx_core::Redactor;

use crate::error::{Result, require};

/// How many messages a channel keeps. Posting one more drops the oldest.
pub const MAX_MESSAGES: usize = 500;

/// What a message is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// A suggestion another agent should consider, e.g. a change to the contract.
    Proposal,
    /// Something the sender needs answered.
    Question,
    /// Something the sender found that others should know.
    Finding,
    /// Where the sender is: started, blocked, done.
    Status,
    /// The contract changed; the body says how.
    ContractUpdate,
}

/// One message on the channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelMessage {
    /// Unique within the space and increasing with every post; dropped messages' ids are never reused.
    pub id: u64,
    /// The sending agent.
    pub from: String,
    /// The receiving agent, or `None` for a broadcast to the whole space.
    pub to: Option<String>,
    /// What the message is for.
    pub kind: MessageKind,
    /// The text, unmasked. [`Channel::list`] masks it.
    pub body: String,
    /// When it was posted, in seconds since the Unix epoch.
    pub created_at: u64,
}

/// A space's channel history, in memory. [`crate::SharedSpaceStore`] keeps it on disk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    next_id: u64,
    #[serde(default)]
    messages: Vec<ChannelMessage>,
}

impl Default for Channel {
    fn default() -> Self {
        Channel {
            next_id: 1,
            messages: Vec::new(),
        }
    }
}

impl Channel {
    /// Post a message and return its id. The oldest messages beyond [`MAX_MESSAGES`] are dropped.
    pub fn post(
        &mut self,
        from: &str,
        to: Option<&str>,
        kind: MessageKind,
        body: &str,
        now: u64,
    ) -> Result<u64> {
        require("from", from)?;
        if let Some(to) = to {
            require("to", to)?;
        }
        require("body", body)?;
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.messages.push(ChannelMessage {
            id,
            from: from.to_owned(),
            to: to.map(str::to_owned),
            kind,
            body: body.to_owned(),
            created_at: now,
        });
        let excess = self.messages.len().saturating_sub(MAX_MESSAGES);
        self.messages.drain(..excess);
        Ok(id)
    }

    /// Every message kept, oldest first, unmasked. Use [`Channel::list`] for anything shown to an agent.
    pub fn messages(&self) -> &[ChannelMessage] {
        &self.messages
    }

    /// The messages that pass every filter given, oldest first, with bodies masked by `redactor`.
    ///
    /// - `to`: only messages addressed to this agent, plus broadcasts (which are addressed to everyone).
    /// - `kind`: only messages of this kind.
    /// - `since_id`: only messages with a larger id, for polling for what is new.
    /// - `limit`: at most this many. With `since_id` these are the oldest after it, so polling again from the
    ///   last id returned misses nothing; without it they are the newest.
    pub fn list(
        &self,
        to: Option<&str>,
        kind: Option<MessageKind>,
        since_id: Option<u64>,
        limit: Option<usize>,
        redactor: &Redactor,
    ) -> Vec<ChannelMessage> {
        let matching: Vec<&ChannelMessage> = self
            .messages
            .iter()
            .filter(|m| since_id.is_none_or(|since| m.id > since))
            .filter(|m| kind.is_none_or(|k| m.kind == k))
            .filter(|m| to.is_none_or(|agent| m.to.as_deref().is_none_or(|t| t == agent)))
            .collect();
        let limit = limit.unwrap_or(usize::MAX);
        let start = if since_id.is_some() {
            0
        } else {
            matching.len().saturating_sub(limit)
        };
        matching
            .into_iter()
            .skip(start)
            .take(limit)
            .map(|m| ChannelMessage {
                body: redactor.redact(&m.body).into_owned(),
                ..m.clone()
            })
            .collect()
    }

    /// Raise the next id above every kept message's, so a hand-edited file cannot make ids repeat.
    pub(crate) fn heal_next_id(&mut self) {
        let floor = self
            .messages
            .iter()
            .map(|m| m.id.saturating_add(1))
            .max()
            .unwrap_or(1);
        self.next_id = self.next_id.max(floor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(messages: &[ChannelMessage]) -> Vec<u64> {
        messages.iter().map(|m| m.id).collect()
    }

    fn sample() -> Result<Channel> {
        let mut c = Channel::default();
        c.post("writer-api", None, MessageKind::Status, "started", 1)?;
        c.post(
            "writer-web",
            Some("writer-api"),
            MessageKind::Question,
            "u32 or i64?",
            2,
        )?;
        c.post(
            "writer-api",
            Some("writer-web"),
            MessageKind::Proposal,
            "u32",
            3,
        )?;
        c.post("tests", None, MessageKind::Finding, "flaky test", 4)?;
        Ok(c)
    }

    #[test]
    fn filters_combine_and_broadcasts_reach_everyone() -> Result<()> {
        let c = sample()?;
        let none = Redactor::default();
        assert_eq!(ids(&c.list(None, None, None, None, &none)), [1, 2, 3, 4]);
        assert_eq!(
            ids(&c.list(Some("writer-api"), None, None, None, &none)),
            [1, 2, 4]
        );
        assert_eq!(
            ids(&c.list(None, Some(MessageKind::Proposal), None, None, &none)),
            [3]
        );
        assert_eq!(ids(&c.list(None, None, Some(2), None, &none)), [3, 4]);
        assert_eq!(
            ids(&c.list(Some("writer-web"), None, Some(1), None, &none)),
            [3, 4]
        );
        Ok(())
    }

    #[test]
    fn limit_takes_the_newest_or_the_oldest_after_since_id() -> Result<()> {
        let c = sample()?;
        let none = Redactor::default();
        assert_eq!(ids(&c.list(None, None, None, Some(2), &none)), [3, 4]);
        assert_eq!(ids(&c.list(None, None, Some(1), Some(2), &none)), [2, 3]);
        assert!(c.list(None, None, None, Some(0), &none).is_empty());
        Ok(())
    }

    #[test]
    fn history_is_capped_and_ids_keep_counting() -> Result<()> {
        let mut c = Channel::default();
        for i in 0..MAX_MESSAGES + 7 {
            c.post("a", None, MessageKind::Status, &format!("tick {i}"), 0)?;
        }
        assert_eq!(c.messages().len(), MAX_MESSAGES);
        assert_eq!(c.messages()[0].id, 8);
        assert_eq!(
            c.messages().last().map(|m| m.id),
            Some((MAX_MESSAGES + 7) as u64)
        );
        Ok(())
    }

    #[test]
    fn listed_bodies_are_masked() -> Result<()> {
        let mut c = Channel::default();
        c.post(
            "a",
            None,
            MessageKind::Finding,
            "token is ghp_secret_value",
            0,
        )?;
        let r = Redactor::new([("GH_TOKEN".to_owned(), "ghp_secret_value".to_owned())]);
        let listed = c.list(None, None, None, None, &r);
        assert_eq!(listed[0].body, "token is [REDACTED:GH_TOKEN]");
        assert_eq!(c.messages()[0].body, "token is ghp_secret_value");
        Ok(())
    }

    #[test]
    fn blank_sender_recipient_or_body_is_refused() {
        let mut c = Channel::default();
        assert!(c.post("", None, MessageKind::Status, "x", 0).is_err());
        assert!(c.post("a", Some(" "), MessageKind::Status, "x", 0).is_err());
        assert!(c.post("a", None, MessageKind::Status, "", 0).is_err());
        assert!(c.messages().is_empty());
    }
}
