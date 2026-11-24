//! The space's Shared Contract: the API shape, event payload or schema the writers of different repos agree on.
//!
//! Each revision bumps [`Contract::version`] and clears every agreement but the author's, so agents must
//! re-agree to what actually changed.

use serde::{Deserialize, Serialize};
use xctx_core::Redactor;

use crate::error::{ChannelError, Result, require};

/// One revision of a space's contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contract {
    /// Starts at 1 and goes up by one with every revision.
    pub version: u32,
    /// A short name, e.g. `rate-limit headers`.
    pub title: String,
    /// What the contract is for, in a sentence or two.
    pub summary: String,
    /// The agreed shape itself: an API signature, an event payload, a schema.
    pub spec: String,
    /// The agent that wrote this revision.
    pub author: String,
    /// Agents that agreed to this revision, in the order they agreed. Always starts with the author.
    pub agreed_by: Vec<String>,
    /// When this revision was written or last agreed to, in seconds since the Unix epoch.
    pub updated_at: u64,
}

/// The text of a contract revision, as an agent proposes it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContractDraft {
    /// See [`Contract::title`]. Must not be blank.
    pub title: String,
    /// See [`Contract::summary`].
    pub summary: String,
    /// See [`Contract::spec`]. Must not be blank.
    pub spec: String,
}

impl Contract {
    /// The revision after `previous` (version 1 when there is none), written by `author` and agreed only by them.
    ///
    /// Returns `None` when `draft` has the same title, summary and spec as `previous`: nothing changed, so the
    /// version and the agreements stand.
    pub fn revise(
        previous: Option<&Contract>,
        author: &str,
        draft: ContractDraft,
        now: u64,
    ) -> Result<Option<Contract>> {
        require("author", author)?;
        require("title", &draft.title)?;
        require("spec", &draft.spec)?;
        if let Some(p) = previous
            && p.title == draft.title
            && p.summary == draft.summary
            && p.spec == draft.spec
        {
            return Ok(None);
        }
        Ok(Some(Contract {
            version: previous.map_or(1, |p| p.version.saturating_add(1)),
            title: draft.title,
            summary: draft.summary,
            spec: draft.spec,
            author: author.to_owned(),
            agreed_by: vec![author.to_owned()],
            updated_at: now,
        }))
    }

    /// Record that `agent` agrees to this revision. Agreeing twice changes nothing; returns whether this call
    /// added the agreement. Same as [`Contract::agree_version`] without a version check.
    pub fn agree(&mut self, agent: &str, now: u64) -> Result<bool> {
        self.agree_version(agent, None, now)
    }

    /// Record that `agent` agrees to this revision, but only if it is `expected_version` (when given): the
    /// revision the agent actually read. Fails with [`ChannelError::StaleContract`] when the contract was revised
    /// since, so an agent cannot agree to text it has not seen. Agreeing twice changes nothing; returns whether
    /// this call added the agreement.
    pub fn agree_version(
        &mut self,
        agent: &str,
        expected_version: Option<u32>,
        now: u64,
    ) -> Result<bool> {
        require("agent", agent)?;
        if let Some(given) = expected_version
            && given != self.version
        {
            return Err(ChannelError::StaleContract {
                current: self.version,
                given,
            });
        }
        if self.agreed_by.iter().any(|a| a == agent) {
            return Ok(false);
        }
        self.agreed_by.push(agent.to_owned());
        self.updated_at = now;
        Ok(true)
    }

    /// True when `agent` agreed to this revision.
    pub fn is_agreed_by(&self, agent: &str) -> bool {
        self.agreed_by.iter().any(|a| a == agent)
    }

    /// A copy with every secret `redactor` knows masked in all text fields, for showing to an agent or the UI.
    pub fn redacted(&self, redactor: &Redactor) -> Contract {
        Contract {
            version: self.version,
            title: redactor.redact(&self.title).into_owned(),
            summary: redactor.redact(&self.summary).into_owned(),
            spec: redactor.redact(&self.spec).into_owned(),
            author: redactor.redact(&self.author).into_owned(),
            agreed_by: self
                .agreed_by
                .iter()
                .map(|a| redactor.redact(a).into_owned())
                .collect(),
            updated_at: self.updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(spec: &str) -> ContractDraft {
        ContractDraft {
            title: "rate-limit headers".into(),
            summary: "what the api returns when it limits".into(),
            spec: spec.into(),
        }
    }

    #[test]
    fn revisions_count_up_and_reset_agreement_to_the_author() -> Result<()> {
        let first = Contract::revise(None, "writer-api", draft("X-RateLimit: int"), 10)?
            .ok_or(ChannelError::NoContract)?;
        assert_eq!(first.version, 1);
        assert_eq!(first.agreed_by, ["writer-api"]);

        let mut agreed = first.clone();
        assert!(agreed.agree("writer-web", 11)?);
        assert!(
            !agreed.agree("writer-web", 12)?,
            "agreeing twice is a no-op"
        );
        assert_eq!(agreed.agreed_by, ["writer-api", "writer-web"]);
        assert_eq!(agreed.updated_at, 11);

        let second = Contract::revise(Some(&agreed), "writer-web", draft("X-RateLimit: u32"), 20)?
            .ok_or(ChannelError::NoContract)?;
        assert_eq!(second.version, 2);
        assert_eq!(second.author, "writer-web");
        assert_eq!(second.agreed_by, ["writer-web"]);
        assert!(!second.is_agreed_by("writer-api"));
        Ok(())
    }

    #[test]
    fn agreeing_to_a_version_checks_it_is_current() -> Result<()> {
        let first = Contract::revise(None, "writer-api", draft("X-RateLimit: int"), 1)?
            .ok_or(ChannelError::NoContract)?;
        let mut second =
            Contract::revise(Some(&first), "writer-api", draft("X-RateLimit: u32"), 2)?
                .ok_or(ChannelError::NoContract)?;

        assert!(matches!(
            second.agree_version("writer-web", Some(1), 3),
            Err(ChannelError::StaleContract {
                current: 2,
                given: 1
            })
        ));
        assert!(!second.is_agreed_by("writer-web"));
        assert_eq!(second.updated_at, 2, "a stale agreement changes nothing");

        assert!(second.agree_version("writer-web", Some(2), 4)?);
        assert!(
            second.agree_version("tests", None, 5)?,
            "no version agrees to the current one"
        );
        assert_eq!(second.agreed_by, ["writer-api", "writer-web", "tests"]);
        Ok(())
    }

    #[test]
    fn an_unchanged_draft_is_not_a_revision() -> Result<()> {
        let first = Contract::revise(None, "a", draft("x"), 1)?.ok_or(ChannelError::NoContract)?;
        assert_eq!(Contract::revise(Some(&first), "b", draft("x"), 2)?, None);
        Ok(())
    }

    #[test]
    fn blank_author_title_or_spec_is_refused() {
        assert!(matches!(
            Contract::revise(None, " ", draft("x"), 1),
            Err(ChannelError::EmptyField("author"))
        ));
        assert!(matches!(
            Contract::revise(None, "a", draft(""), 1),
            Err(ChannelError::EmptyField("spec"))
        ));
        let mut untitled = draft("x");
        untitled.title.clear();
        assert!(matches!(
            Contract::revise(None, "a", untitled, 1),
            Err(ChannelError::EmptyField("title"))
        ));
    }

    #[test]
    fn redacted_masks_secrets_in_every_text_field() -> Result<()> {
        let redactor = Redactor::new([("API_KEY".to_owned(), "sk-live-123".to_owned())]);
        let contract = Contract::revise(
            None,
            "writer",
            ContractDraft {
                title: "uses sk-live-123".into(),
                summary: "auth with sk-live-123".into(),
                spec: "Authorization: Bearer sk-live-123".into(),
            },
            1,
        )?
        .ok_or(ChannelError::NoContract)?;
        let shown = contract.redacted(&redactor);
        let text = format!("{} {} {}", shown.title, shown.summary, shown.spec);
        assert!(!text.contains("sk-live-123"));
        assert_eq!(shown.spec, "Authorization: Bearer [REDACTED:API_KEY]");
        assert_eq!(shown.version, contract.version);
        Ok(())
    }
}
