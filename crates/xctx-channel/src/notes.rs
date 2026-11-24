//! Findings and shared notes: what helpers (tests, reviewer, probe) report, and what any agent wants the space to
//! remember.
//!
//! Both are bounded ([`MAX_FINDINGS`], [`MAX_NOTES`]) so an agent reporting in a loop cannot grow memory or
//! `shared.json` without limit.

use serde::{Deserialize, Serialize};
use xctx_core::Redactor;

use crate::error::{ChannelError, Result, require};

/// How many findings a space keeps. Adding one more drops the oldest resolved finding, or the oldest finding when
/// none is resolved.
pub const MAX_FINDINGS: usize = 500;

/// How many notes a space keeps. Adding one more drops the oldest.
pub const MAX_NOTES: usize = 500;

/// How serious a finding is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingSeverity {
    /// Worth knowing; nothing is wrong.
    Info,
    /// Probably wrong, or will be.
    Warning,
    /// Wrong: a failing test, a broken contract.
    Error,
}

/// Something a helper found, optionally pinned to a repo and file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Unique among the space's findings.
    pub id: u64,
    /// The agent that reported it.
    pub author: String,
    /// How serious it is.
    pub severity: FindingSeverity,
    /// The repo it concerns, by name.
    pub repo: Option<String>,
    /// The file it concerns, relative to the repo.
    pub file: Option<String>,
    /// What was found.
    pub summary: String,
    /// True once someone marked it dealt with.
    pub resolved: bool,
    /// When it was reported, in seconds since the Unix epoch.
    pub created_at: u64,
}

impl Finding {
    /// A copy with every secret `redactor` knows masked in all text fields.
    pub fn redacted(&self, redactor: &Redactor) -> Finding {
        let mask = |s: &str| redactor.redact(s).into_owned();
        Finding {
            author: mask(&self.author),
            repo: self.repo.as_deref().map(mask),
            file: self.file.as_deref().map(mask),
            summary: mask(&self.summary),
            ..self.clone()
        }
    }
}

/// A free-form note shared with the space.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    /// Unique among the space's notes.
    pub id: u64,
    /// The agent that wrote it.
    pub author: String,
    /// The text, unmasked. [`SharedNotes::notes`] masks it.
    pub body: String,
    /// When it was written, in seconds since the Unix epoch.
    pub created_at: u64,
}

impl Note {
    /// A copy with every secret `redactor` knows masked in all text fields.
    pub fn redacted(&self, redactor: &Redactor) -> Note {
        Note {
            author: redactor.redact(&self.author).into_owned(),
            body: redactor.redact(&self.body).into_owned(),
            ..self.clone()
        }
    }
}

/// A space's findings and notes, in memory. [`crate::SharedSpaceStore`] keeps them on disk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedNotes {
    next_finding_id: u64,
    next_note_id: u64,
    #[serde(default)]
    findings: Vec<Finding>,
    #[serde(default)]
    notes: Vec<Note>,
}

impl Default for SharedNotes {
    fn default() -> Self {
        SharedNotes {
            next_finding_id: 1,
            next_note_id: 1,
            findings: Vec::new(),
            notes: Vec::new(),
        }
    }
}

/// Treat a blank optional field as absent.
fn non_blank(value: Option<&str>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty()).map(str::to_owned)
}

impl SharedNotes {
    /// Record an unresolved finding and return its id. A blank `repo` or `file` counts as none. Beyond
    /// [`MAX_FINDINGS`], the oldest resolved finding is dropped first, then the oldest unresolved one.
    pub fn add_finding(
        &mut self,
        author: &str,
        severity: FindingSeverity,
        repo: Option<&str>,
        file: Option<&str>,
        summary: &str,
        now: u64,
    ) -> Result<u64> {
        require("author", author)?;
        require("summary", summary)?;
        let id = self.next_finding_id;
        self.next_finding_id = self.next_finding_id.saturating_add(1);
        self.findings.push(Finding {
            id,
            author: author.to_owned(),
            severity,
            repo: non_blank(repo),
            file: non_blank(file),
            summary: summary.to_owned(),
            resolved: false,
            created_at: now,
        });
        while self.findings.len() > MAX_FINDINGS {
            let oldest = self.findings.iter().position(|f| f.resolved).unwrap_or(0);
            self.findings.remove(oldest);
        }
        Ok(id)
    }

    /// Mark a finding resolved. Resolving it again changes nothing; returns whether this call resolved it.
    pub fn resolve_finding(&mut self, id: u64) -> Result<bool> {
        let finding = self
            .findings
            .iter_mut()
            .find(|f| f.id == id)
            .ok_or(ChannelError::FindingNotFound(id))?;
        let changed = !finding.resolved;
        finding.resolved = true;
        Ok(changed)
    }

    /// Record a note and return its id. The oldest notes beyond [`MAX_NOTES`] are dropped.
    pub fn add_note(&mut self, author: &str, body: &str, now: u64) -> Result<u64> {
        require("author", author)?;
        require("body", body)?;
        let id = self.next_note_id;
        self.next_note_id = self.next_note_id.saturating_add(1);
        self.notes.push(Note {
            id,
            author: author.to_owned(),
            body: body.to_owned(),
            created_at: now,
        });
        let excess = self.notes.len().saturating_sub(MAX_NOTES);
        self.notes.drain(..excess);
        Ok(id)
    }

    /// The findings, oldest first and masked by `redactor`; only unresolved ones when `unresolved_only`.
    pub fn findings(&self, unresolved_only: bool, redactor: &Redactor) -> Vec<Finding> {
        self.findings
            .iter()
            .filter(|f| !unresolved_only || !f.resolved)
            .map(|f| f.redacted(redactor))
            .collect()
    }

    /// The notes, oldest first and masked by `redactor`.
    pub fn notes(&self, redactor: &Redactor) -> Vec<Note> {
        self.notes.iter().map(|n| n.redacted(redactor)).collect()
    }

    /// Raise the next ids above every kept item's, so a hand-edited file cannot make ids repeat.
    pub(crate) fn heal_next_ids(&mut self) {
        let finding_floor = self.findings.iter().map(|f| f.id.saturating_add(1)).max();
        let note_floor = self.notes.iter().map(|n| n.id.saturating_add(1)).max();
        self.next_finding_id = self.next_finding_id.max(finding_floor.unwrap_or(1));
        self.next_note_id = self.next_note_id.max(note_floor.unwrap_or(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn findings_resolve_once_and_filter_by_state() -> Result<()> {
        let mut n = SharedNotes::default();
        let a = n.add_finding(
            "tests",
            FindingSeverity::Error,
            Some("api"),
            Some("src/limit.rs"),
            "limit test fails",
            1,
        )?;
        let b = n.add_finding("reviewer", FindingSeverity::Info, Some(""), None, "ok", 2)?;
        assert_eq!((a, b), (1, 2));
        assert!(n.resolve_finding(a)?);
        assert!(!n.resolve_finding(a)?);
        assert!(matches!(
            n.resolve_finding(99),
            Err(ChannelError::FindingNotFound(99))
        ));
        let none = Redactor::default();
        assert_eq!(n.findings(false, &none).len(), 2);
        let open = n.findings(true, &none);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, b);
        assert_eq!(open[0].repo, None, "a blank repo counts as none");
        Ok(())
    }

    #[test]
    fn findings_and_notes_are_capped_dropping_resolved_findings_first() -> Result<()> {
        let mut n = SharedNotes::default();
        for i in 0..MAX_FINDINGS {
            n.add_finding(
                "tests",
                FindingSeverity::Info,
                None,
                None,
                &format!("f{i}"),
                0,
            )?;
        }
        n.resolve_finding(10)?;
        n.resolve_finding(20)?;
        n.add_finding("tests", FindingSeverity::Info, None, None, "one more", 0)?;
        n.add_finding("tests", FindingSeverity::Info, None, None, "two more", 0)?;
        let none = Redactor::default();
        let ids: Vec<u64> = n.findings(false, &none).iter().map(|f| f.id).collect();
        assert_eq!(ids.len(), MAX_FINDINGS);
        assert!(
            !ids.contains(&10) && !ids.contains(&20),
            "resolved go first"
        );
        assert_eq!(
            ids.first(),
            Some(&1),
            "unresolved stay while resolved remain"
        );

        let last = n.add_finding("tests", FindingSeverity::Info, None, None, "three", 0)?;
        let ids: Vec<u64> = n.findings(false, &none).iter().map(|f| f.id).collect();
        assert_eq!(ids.len(), MAX_FINDINGS);
        assert_eq!(ids.first(), Some(&2), "then the oldest unresolved goes");
        assert_eq!(ids.last(), Some(&last));

        for i in 0..MAX_NOTES + 3 {
            n.add_note("writer", &format!("n{i}"), 0)?;
        }
        let notes = n.notes(&none);
        assert_eq!(notes.len(), MAX_NOTES);
        assert_eq!(notes[0].id, 4);
        assert_eq!(notes.last().map(|n| n.id), Some((MAX_NOTES + 3) as u64));
        Ok(())
    }

    #[test]
    fn findings_and_notes_are_masked() -> Result<()> {
        let r = Redactor::new([("DB_PASSWORD".to_owned(), "hunter22".to_owned())]);
        let mut n = SharedNotes::default();
        n.add_finding(
            "probe",
            FindingSeverity::Warning,
            Some("api"),
            Some("config/hunter22.env"),
            "login with hunter22 works",
            1,
        )?;
        n.add_note("writer", "password is hunter22", 2)?;
        let f = &n.findings(false, &r)[0];
        assert_eq!(f.summary, "login with [REDACTED:DB_PASSWORD] works");
        assert_eq!(f.file.as_deref(), Some("config/[REDACTED:DB_PASSWORD].env"));
        assert_eq!(n.notes(&r)[0].body, "password is [REDACTED:DB_PASSWORD]");
        Ok(())
    }

    #[test]
    fn blank_fields_are_refused() {
        let mut n = SharedNotes::default();
        assert!(
            n.add_finding("", FindingSeverity::Info, None, None, "x", 0)
                .is_err()
        );
        assert!(
            n.add_finding("a", FindingSeverity::Info, None, None, " ", 0)
                .is_err()
        );
        assert!(n.add_note("a", "", 0).is_err());
        assert_eq!(n, SharedNotes::default());
    }
}
