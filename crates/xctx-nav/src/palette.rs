//! Command palette model: prefix parsing (`>`, `@`, `#`), case-insensitive fuzzy subsequence
//! matching with highlight indices, and bounded recent-command ordering.

use std::collections::VecDeque;

/// Default maximum number of recent command ids kept by [`Recents`].
pub const DEFAULT_MAX_RECENTS: usize = 32;

/// Category of an entry in the command palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PaletteKind {
    /// A keymap or shell action (`>` prefix).
    Action,
    /// A living space (`#` prefix).
    Space,
    /// An agent in a space (`@` prefix).
    Agent,
    /// A mounted or available plugin.
    Plugin,
}

/// Scope filter parsed from the leading character of a palette query.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PalettePrefix {
    /// Search across every [`PaletteKind`].
    All,
    /// Leading `>`: search [`PaletteKind::Action`] only.
    Actions,
    /// Leading `@`: search [`PaletteKind::Agent`] only.
    Agents,
    /// Leading `#`: search [`PaletteKind::Space`] only.
    Spaces,
}

impl PalettePrefix {
    /// True when `kind` is allowed under this prefix filter.
    pub fn matches_kind(self, kind: PaletteKind) -> bool {
        match self {
            Self::All => true,
            Self::Actions => kind == PaletteKind::Action,
            Self::Agents => kind == PaletteKind::Agent,
            Self::Spaces => kind == PaletteKind::Space,
        }
    }
}

/// One searchable entry in the command palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaletteItem {
    /// Stable identifier (used by [`Recents`] and action dispatch).
    pub id: String,
    /// Entry category.
    pub kind: PaletteKind,
    /// Primary display label matched first.
    pub title: String,
    /// Optional secondary text (e.g. space name, description).
    pub subtitle: Option<String>,
    /// Optional key chord badge shown beside the item (e.g. `"⌘⇧I"`).
    pub key_chord: Option<String>,
    /// Extra search terms matched as a fallback when `title` does not match.
    pub keywords: Vec<String>,
}

impl PaletteItem {
    /// Construct a minimal palette item with `id`, `kind`, and `title`.
    pub fn new(id: impl Into<String>, kind: PaletteKind, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind,
            title: title.into(),
            subtitle: None,
            key_chord: None,
            keywords: Vec::new(),
        }
    }

    /// Attach a subtitle.
    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// Attach a key chord badge.
    pub fn with_key_chord(mut self, key_chord: impl Into<String>) -> Self {
        self.key_chord = Some(key_chord.into());
        self
    }

    /// Attach extra search keywords.
    pub fn with_keywords(mut self, keywords: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.keywords = keywords.into_iter().map(Into::into).collect();
        self
    }
}

/// One ranked search hit returned by [`search`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaletteMatch {
    /// Index into the `items` slice passed to [`search`].
    pub index: usize,
    /// Ranking score (higher is better).
    pub score: i32,
    /// Character indices in [`PaletteItem::title`] that matched the query, in ascending order.
    /// Empty when the query was empty or matched only via subtitle/keywords.
    pub title_indices: Vec<usize>,
}

/// Bounded most-recently-used list of palette item ids.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recents {
    max_len: usize,
    ids: VecDeque<String>,
}

impl Default for Recents {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_RECENTS)
    }
}

impl Recents {
    /// Create an empty recents list holding at most `max_len` ids.
    pub fn new(max_len: usize) -> Self {
        Self {
            max_len,
            ids: VecDeque::with_capacity(max_len.min(DEFAULT_MAX_RECENTS)),
        }
    }

    /// Record `id` as the most recent item, moving it to the front if already present and evicting
    /// the oldest item when full.
    pub fn record(&mut self, id: impl Into<String>) {
        if self.max_len == 0 {
            return;
        }
        let id = id.into();
        if let Some(pos) = self.ids.iter().position(|existing| existing == &id) {
            self.ids.remove(pos);
        }
        self.ids.push_front(id);
        while self.ids.len() > self.max_len {
            self.ids.pop_back();
        }
    }

    /// 0-based recency rank of `id` (`Some(0)` is most recent), or `None` if not recorded.
    pub fn rank(&self, id: &str) -> Option<usize> {
        self.ids.iter().position(|existing| existing == id)
    }

    /// Recorded ids from most recent to oldest.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.ids.iter().map(String::as_str)
    }

    /// Number of recorded ids.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// True when no ids have been recorded.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

/// Split `raw` into a [`PalettePrefix`] and the trimmed search needle.
pub fn parse_query(raw: &str) -> (PalettePrefix, &str) {
    let trimmed = raw.trim_start();
    if let Some(rest) = trimmed.strip_prefix('>') {
        (PalettePrefix::Actions, rest.trim())
    } else if let Some(rest) = trimmed.strip_prefix('@') {
        (PalettePrefix::Agents, rest.trim())
    } else if let Some(rest) = trimmed.strip_prefix('#') {
        (PalettePrefix::Spaces, rest.trim())
    } else {
        (PalettePrefix::All, trimmed.trim())
    }
}

/// Case-insensitive fuzzy subsequence match of `needle` against `candidate`.
///
/// Returns `Some((score, matched_char_indices))` when every non-whitespace character of `needle`
/// appears in `candidate` in order, favouring:
/// - prefix matches at index 0,
/// - matches at word boundaries (start of string, after whitespace/`-`/`_`/`/`/`.`/`:`, or camelCase
///   uppercase transitions),
/// - consecutive character runs,
/// - tighter spans and shorter candidates.
pub fn fuzzy_score(needle: &str, candidate: &str) -> Option<(i32, Vec<usize>)> {
    let needle_chars: Vec<char> = needle
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if needle_chars.is_empty() {
        return Some((0, Vec::new()));
    }

    let cand_orig: Vec<char> = candidate.chars().collect();
    let cand_lower: Vec<char> = cand_orig
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();

    if needle_chars.len() > cand_lower.len() {
        return None;
    }

    // Greedy forward pass with word-boundary lookahead: for each needle char, prefer the earliest
    // match that lands on a word boundary or continues a consecutive run, as long as the remainder
    // of the needle can still match.
    let mut indices = Vec::with_capacity(needle_chars.len());
    let mut cursor = 0usize;

    for (ni, &nc) in needle_chars.iter().enumerate() {
        let mut best_pos: Option<(usize, i32)> = None;
        let prev = indices.last().copied();
        for ci in cursor..cand_lower.len() {
            if cand_lower[ci] != nc {
                continue;
            }
            // Verify remaining needle chars can still match after `ci`.
            if !can_match_tail(&needle_chars[ni + 1..], &cand_lower[ci + 1..]) {
                break;
            }
            let local_bonus = char_bonus(&cand_orig, ci, prev);
            let replace = match best_pos {
                None => true,
                Some((_, best_bonus)) => local_bonus > best_bonus,
            };
            if replace {
                best_pos = Some((ci, local_bonus));
            }
            // If we hit a consecutive match or exact start-of-string match, take it immediately.
            if prev.is_some_and(|p| p + 1 == ci) || (ni == 0 && ci == 0) {
                break;
            }
        }
        let (chosen, _) = best_pos?;
        indices.push(chosen);
        cursor = chosen + 1;
    }

    let mut score: i32 = 0;
    let mut prev: Option<usize> = None;
    let mut consecutive_run: i32 = 0;
    for &ci in &indices {
        score += 10;
        if let Some(p) = prev {
            if p + 1 == ci {
                consecutive_run += 1;
                score += 12 + consecutive_run * 4;
            } else {
                consecutive_run = 0;
                let gap = (ci - p - 1) as i32;
                score -= gap.min(12);
            }
        } else if ci == 0 {
            score += 24;
        }
        if is_word_boundary(&cand_orig, ci) {
            score += 16;
        }
        prev = Some(ci);
    }

    // Slight preference for shorter candidates when scores otherwise tie.
    score -= (cand_orig.len().saturating_sub(indices.len()) as i32).min(40) / 2;

    Some((score, indices))
}

/// Filter and rank `items` for `query`, ordering recent commands first when the needle is empty
/// and giving recent commands a small tie-breaking boost when filtering.
pub fn search(items: &[PaletteItem], query: &str, recents: &Recents) -> Vec<PaletteMatch> {
    let (prefix, needle) = parse_query(query);

    let mut hits: Vec<(PaletteMatch, Option<usize>)> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if !prefix.matches_kind(item.kind) {
            continue;
        }
        let recent_rank = recents.rank(&item.id);

        if needle.is_empty() {
            hits.push((
                PaletteMatch {
                    index,
                    score: 0,
                    title_indices: Vec::new(),
                },
                recent_rank,
            ));
            continue;
        }

        // Match against title first; fall back to subtitle and keywords at a lower tier so
        // direct title matches always rank above metadata-only matches.
        let matched = if let Some((title_score, title_indices)) = fuzzy_score(needle, &item.title) {
            Some((title_score + 100, title_indices))
        } else {
            let mut best_meta: Option<i32> = None;
            if let Some(sub) = &item.subtitle
                && let Some((s, _)) = fuzzy_score(needle, sub)
            {
                best_meta = Some(best_meta.map_or(s, |cur| cur.max(s)));
            }
            for kw in &item.keywords {
                if let Some((s, _)) = fuzzy_score(needle, kw) {
                    best_meta = Some(best_meta.map_or(s, |cur| cur.max(s)));
                }
            }
            best_meta.map(|s| (s, Vec::new()))
        };

        if let Some((mut score, title_indices)) = matched {
            if let Some(rank) = recent_rank {
                // Small recency boost (max +12 for most recent, decaying by 1 per slot).
                score += (12i32 - rank as i32).max(1);
            }
            hits.push((
                PaletteMatch {
                    index,
                    score,
                    title_indices,
                },
                recent_rank,
            ));
        }
    }

    if needle.is_empty() {
        hits.sort_by(|(a, a_rec), (b, b_rec)| match (a_rec, b_rec) {
            (Some(ra), Some(rb)) => ra.cmp(rb),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.index.cmp(&b.index),
        });
    } else {
        hits.sort_by(|(a, a_rec), (b, b_rec)| {
            b.score
                .cmp(&a.score)
                .then_with(|| match (a_rec, b_rec) {
                    (Some(ra), Some(rb)) => ra.cmp(rb),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                })
                .then_with(|| items[a.index].title.cmp(&items[b.index].title))
        });
    }

    hits.into_iter().map(|(m, _)| m).collect()
}

fn can_match_tail(needle: &[char], cand: &[char]) -> bool {
    let mut ci = 0usize;
    for &nc in needle {
        let mut found = false;
        while ci < cand.len() {
            if cand[ci] == nc {
                ci += 1;
                found = true;
                break;
            }
            ci += 1;
        }
        if !found {
            return false;
        }
    }
    true
}

fn is_word_boundary(chars: &[char], idx: usize) -> bool {
    if idx == 0 {
        return true;
    }
    let prev = chars[idx - 1];
    let curr = chars[idx];
    prev.is_whitespace()
        || matches!(prev, '-' | '_' | '/' | '.' | ':' | '·')
        || (prev.is_lowercase() && curr.is_uppercase())
}

fn char_bonus(chars: &[char], idx: usize, prev: Option<usize>) -> i32 {
    let mut bonus = 0;
    if prev.is_some_and(|p| p + 1 == idx) {
        bonus += 30;
    }
    if idx == 0 {
        bonus += 25;
    } else if is_word_boundary(chars, idx) {
        bonus += 20;
    }
    bonus
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_items() -> Vec<PaletteItem> {
        vec![
            PaletteItem::new("act.inbox", PaletteKind::Action, "Open Inbox")
                .with_key_chord("⌘⇧I")
                .with_keywords(["gates", "approvals"]),
            PaletteItem::new("act.inspector", PaletteKind::Action, "Toggle Inspector")
                .with_key_chord("⌘I"),
            PaletteItem::new("act.ship", PaletteKind::Action, "Ship Space")
                .with_subtitle("Push branch and open PR"),
            PaletteItem::new("space.checkout", PaletteKind::Space, "checkout-retry")
                .with_subtitle("space/checkout-retry"),
            PaletteItem::new("agent.writer", PaletteKind::Agent, "writer · api")
                .with_subtitle("checkout-retry"),
            PaletteItem::new("plugin.logs", PaletteKind::Plugin, "Logs")
                .with_subtitle("Tail service output"),
        ]
    }

    #[test]
    fn parse_query_recognises_all_prefixes() {
        assert_eq!(parse_query("ship"), (PalettePrefix::All, "ship"));
        assert_eq!(parse_query("  > ship "), (PalettePrefix::Actions, "ship"));
        assert_eq!(parse_query("@writer"), (PalettePrefix::Agents, "writer"));
        assert_eq!(
            parse_query("#checkout"),
            (PalettePrefix::Spaces, "checkout")
        );
        assert_eq!(parse_query(">"), (PalettePrefix::Actions, ""));
    }

    #[test]
    fn prefixes_filter_by_kind() {
        let items = sample_items();
        let recents = Recents::default();

        let actions = search(&items, ">", &recents);
        assert!(
            actions
                .iter()
                .all(|m| items[m.index].kind == PaletteKind::Action)
        );
        assert_eq!(actions.len(), 3);

        let agents = search(&items, "@", &recents);
        assert_eq!(agents.len(), 1);
        assert_eq!(items[agents[0].index].id, "agent.writer");

        let spaces = search(&items, "#", &recents);
        assert_eq!(spaces.len(), 1);
        assert_eq!(items[spaces[0].index].id, "space.checkout");
    }

    #[test]
    fn fuzzy_matcher_ranks_prefix_and_word_boundaries_and_returns_indices() {
        let items = sample_items();
        let recents = Recents::default();

        let hits = search(&items, "oi", &recents);
        assert!(!hits.is_empty());
        assert_eq!(items[hits[0].index].id, "act.inbox");
        // 'O' at 0 and 'I' at 5 ("Open Inbox")
        assert_eq!(hits[0].title_indices, vec![0, 5]);

        // Keyword-only match still finds Open Inbox with empty title_indices
        let kw_hits = search(&items, "approvals", &recents);
        assert_eq!(kw_hits.len(), 1);
        assert_eq!(items[kw_hits[0].index].id, "act.inbox");
        assert!(kw_hits[0].title_indices.is_empty());

        // Non-matching query returns empty
        assert!(search(&items, "zzzzzz", &recents).is_empty());
    }

    #[test]
    fn recents_orders_empty_query_and_deduplicates_and_caps() {
        let items = sample_items();
        let mut recents = Recents::new(2);
        recents.record("act.ship");
        recents.record("space.checkout");
        recents.record("act.ship"); // moves to front, no duplicate
        assert_eq!(recents.len(), 2);
        assert_eq!(recents.rank("act.ship"), Some(0));
        assert_eq!(recents.rank("space.checkout"), Some(1));

        // Recording a third evicts the oldest ("space.checkout")
        recents.record("plugin.logs");
        assert_eq!(recents.rank("plugin.logs"), Some(0));
        assert_eq!(recents.rank("act.ship"), Some(1));
        assert_eq!(recents.rank("space.checkout"), None);

        let hits = search(&items, "", &recents);
        assert_eq!(items[hits[0].index].id, "plugin.logs");
        assert_eq!(items[hits[1].index].id, "act.ship");
    }
}
