//! The repo index: the git repositories under the folders the user chose, with what is known about each, and a
//! task-to-repositories search that drafts a space and says why each repository is in it.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::git;
use crate::space::{DraftRepo, SpaceDraft};

/// How deep under a root folder [`RepoIndex`] looks for repositories.
pub const DEFAULT_SCAN_DEPTH: usize = 4;

/// How many repositories [`RepoIndex::draft`] proposes at most.
pub const DRAFT_LIMIT: usize = 5;

const MAX_FILES: usize = 30_000;
const MAX_README_BYTES: u64 = 8 * 1024;
const MAX_TOP_LEVEL_FILES: usize = 40;
const MAX_DESCRIPTION_CHARS: usize = 240;
const RECENT_COUNT: usize = 5;
const LANGUAGE_COUNT: usize = 3;

/// Folders never worth descending into.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".build",
    "target",
    "dist",
    "build",
    ".git",
    "Library",
    "vendor",
    ".venv",
    "venv",
    "Pods",
    "DerivedData",
    ".next",
    ".cache",
    ".Trash",
    "Applications",
];

/// Words too common in tasks to say anything about a repository.
const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "into", "when", "make", "add", "fix",
    "our", "all", "are", "not", "can", "use", "new", "get", "set", "should", "need",
];

/// The meaningful words of `text`: lowercased runs of letters and digits, at least three long, without common
/// task words like "the", "add" or "fix". Used for search and for slugs.
pub fn task_words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3 && !STOP_WORDS.contains(w))
        .map(str::to_owned)
        .collect()
}

/// A repository found under a root folder, with what the index knows about it. Everything beyond the path is
/// best effort: a repository with no commits or no README is still indexed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedRepo {
    /// The folder name.
    pub name: String,
    /// The repository's top directory.
    pub path: PathBuf,
    /// The first paragraph of the README, if there is one.
    pub description: Option<String>,
    /// The names at the top of the repository, sorted, directories ending in `/`.
    pub top_level_files: Vec<String>,
    /// The main languages by tracked file count, most first.
    pub languages: Vec<String>,
    /// Local branches, most recently committed first.
    pub recent_branches: Vec<String>,
    /// Subjects of the latest commits on HEAD, newest first.
    pub recent_commits: Vec<String>,
    /// When the git index last changed: a cheap "recently used".
    pub touched: Option<SystemTime>,
    files: Vec<String>,
    files_lower: Vec<String>,
    readme_lower: String,
}

impl IndexedRepo {
    /// Read what can be learned about the repository at `path` without changing it.
    pub fn read(path: &Path) -> IndexedRepo {
        let files: Vec<String> = git::run_git(path, ["ls-files"])
            .map(|out| out.lines().take(MAX_FILES).map(str::to_owned).collect())
            .unwrap_or_default();
        let files_lower = files.iter().map(|f| f.to_lowercase()).collect();
        let readme = read_readme(path);
        let lines = |args: &[&str]| -> Vec<String> {
            git::run_git(path, args)
                .map(|out| {
                    out.lines()
                        .map(str::to_owned)
                        .filter(|l| !l.is_empty())
                        .collect()
                })
                .unwrap_or_default()
        };
        let count = RECENT_COUNT.to_string();
        IndexedRepo {
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: path.to_path_buf(),
            description: describe(&readme),
            top_level_files: top_level_files(path),
            languages: languages(&files),
            recent_branches: lines(&[
                "for-each-ref",
                "--sort=-committerdate",
                "--count",
                &count,
                "--format=%(refname:short)",
                "refs/heads",
            ]),
            recent_commits: lines(&["log", "-n", &count, "--format=%s"]),
            touched: fs::metadata(path.join(".git").join("index"))
                .and_then(|m| m.modified())
                .ok(),
            files,
            files_lower,
            readme_lower: readme.to_lowercase(),
        }
    }

    /// Tracked file paths, as git lists them (at most 30 000).
    pub fn files(&self) -> &[String] {
        &self.files
    }
}

fn read_readme(repo: &Path) -> String {
    for name in [
        "README.md",
        "readme.md",
        "Readme.md",
        "README",
        "README.txt",
        "README.rst",
    ] {
        let Ok(file) = fs::File::open(repo.join(name)) else {
            continue;
        };
        let mut bytes = Vec::new();
        if file.take(MAX_README_BYTES).read_to_end(&mut bytes).is_ok() {
            return String::from_utf8_lossy(&bytes).into_owned();
        }
    }
    String::new()
}

/// The README's first prose paragraph, skipping headings, badges and HTML; the first heading when there is none.
fn describe(readme: &str) -> Option<String> {
    let mut paragraph: Vec<&str> = Vec::new();
    let mut heading = None;
    for line in readme.lines().map(str::trim) {
        let decoration = line.starts_with('#')
            || line.starts_with("![")
            || line.starts_with("[!")
            || line.starts_with('<')
            || line.starts_with("---")
            || line.starts_with("===")
            || line.starts_with("```");
        if line.is_empty() || decoration {
            if heading.is_none() && line.starts_with('#') {
                heading = Some(line.trim_start_matches('#').trim());
            }
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        paragraph.push(line);
    }
    let text = if paragraph.is_empty() {
        heading.filter(|h| !h.is_empty())?.to_owned()
    } else {
        paragraph.join(" ")
    };
    Some(if text.chars().count() > MAX_DESCRIPTION_CHARS {
        let cut: String = text.chars().take(MAX_DESCRIPTION_CHARS).collect();
        format!("{}…", cut.trim_end())
    } else {
        text
    })
}

fn top_level_files(repo: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(repo) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name == ".git" {
                return None;
            }
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            Some(if is_dir { format!("{name}/") } else { name })
        })
        .collect();
    names.sort();
    names.truncate(MAX_TOP_LEVEL_FILES);
    names
}

fn language_of(path: &str) -> Option<&'static str> {
    let ext = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => "Rust",
        "ts" | "tsx" | "mts" | "cts" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "swift" => "Swift",
        "py" => "Python",
        "go" => "Go",
        "rb" => "Ruby",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "c" | "h" => "C",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "C++",
        "cs" => "C#",
        "m" | "mm" => "Objective-C",
        "php" => "PHP",
        "scala" => "Scala",
        "ex" | "exs" => "Elixir",
        "zig" => "Zig",
        "lua" => "Lua",
        "sh" | "bash" | "zsh" => "Shell",
        "vue" => "Vue",
        "svelte" => "Svelte",
        "dart" => "Dart",
        "hs" => "Haskell",
        "ml" | "mli" => "OCaml",
        "clj" | "cljs" => "Clojure",
        "sql" => "SQL",
        _ => return None,
    })
}

fn languages(files: &[String]) -> Vec<String> {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for file in files {
        if let Some(lang) = language_of(file) {
            *counts.entry(lang).or_default() += 1;
        }
    }
    let mut ranked: Vec<(&'static str, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    ranked
        .into_iter()
        .take(LANGUAGE_COUNT)
        .map(|(lang, _)| lang.to_owned())
        .collect()
}

/// The repositories under `root`, at most `depth` folders down. A folder with a `.git` is a repository and is not
/// searched further. Hidden folders, symlinks, build and dependency folders, and the `*-worktrees` folders spaces
/// put their worktrees in are skipped.
pub fn find_repos(root: &Path, depth: usize) -> Vec<PathBuf> {
    if root.join(".git").exists() {
        return vec![root.to_path_buf()];
    }
    if depth == 0 {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            is_dir
                && !name.starts_with('.')
                && !name.ends_with("-worktrees")
                && !SKIP_DIRS.contains(&name.as_str())
        })
        .map(|e| e.path())
        .collect();
    dirs.sort();
    dirs.iter().flat_map(|d| find_repos(d, depth - 1)).collect()
}

/// A repository [`RepoIndex::search`] matched, with how well and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoMatch<'a> {
    /// The repository.
    pub repo: &'a IndexedRepo,
    /// Higher is better. A name match counts most, then the README, file paths and recent work.
    pub score: u32,
    /// What matched, each a short phrase for the user, most telling first.
    pub reasons: Vec<String>,
}

impl RepoMatch<'_> {
    /// The reasons as one line, e.g. `name matches "payments"; files src/limit.ts`.
    pub fn reason(&self) -> String {
        self.reasons.join("; ")
    }
}

/// The repositories under the root folders, and a task-to-repositories search over them.
#[derive(Clone, Debug, Default)]
pub struct RepoIndex {
    roots: Vec<PathBuf>,
    repos: Vec<IndexedRepo>,
}

impl RepoIndex {
    /// An index of `roots`. Nothing is read until [`RepoIndex::scan`].
    pub fn new(roots: Vec<PathBuf>) -> RepoIndex {
        RepoIndex {
            roots,
            repos: Vec::new(),
        }
    }

    /// The root folders.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// The repositories found by the last scan, most recently used first.
    pub fn repos(&self) -> &[IndexedRepo] {
        &self.repos
    }

    /// The indexed repository at `path`.
    pub fn repo(&self, path: &Path) -> Option<&IndexedRepo> {
        self.repos.iter().find(|r| r.path == path)
    }

    /// Find and read every repository under the roots again.
    pub fn scan(&mut self) {
        let mut seen = HashSet::new();
        let paths: Vec<PathBuf> = self
            .roots
            .iter()
            .flat_map(|root| find_repos(root, DEFAULT_SCAN_DEPTH))
            .filter(|p| seen.insert(p.clone()))
            .collect();
        self.repos = paths.iter().map(|p| IndexedRepo::read(p)).collect();
        self.sort();
    }

    fn sort(&mut self) {
        self.repos
            .sort_by(|a, b| b.touched.cmp(&a.touched).then_with(|| a.name.cmp(&b.name)));
    }

    /// Add a root folder and index the repositories under it. Returns false, and changes nothing, when it was
    /// already a root.
    pub fn add_root(&mut self, root: PathBuf) -> bool {
        if self.roots.contains(&root) {
            return false;
        }
        for path in find_repos(&root, DEFAULT_SCAN_DEPTH) {
            if self.repo(&path).is_none() {
                self.repos.push(IndexedRepo::read(&path));
            }
        }
        self.roots.push(root);
        self.sort();
        true
    }

    /// Remove a root folder and forget the repositories under it that no other root holds. Returns false when it
    /// was not a root.
    pub fn remove_root(&mut self, root: &Path) -> bool {
        let before = self.roots.len();
        self.roots.retain(|r| r != root);
        if self.roots.len() == before {
            return false;
        }
        let roots = &self.roots;
        self.repos.retain(|repo| {
            !repo.path.starts_with(root) || roots.iter().any(|r| repo.path.starts_with(r))
        });
        true
    }

    /// The repositories `task` probably touches, best first, at most `limit`. Ties go to the most recently used.
    pub fn search(&self, task: &str, limit: usize) -> Vec<RepoMatch<'_>> {
        let mut words: Vec<String> = Vec::new();
        for word in task_words(task) {
            if !words.contains(&word) {
                words.push(word);
            }
        }
        if words.is_empty() {
            return Vec::new();
        }
        let mut matches: Vec<RepoMatch<'_>> = self
            .repos
            .iter()
            .filter_map(|repo| score_repo(repo, &words))
            .collect();
        matches.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| b.repo.touched.cmp(&a.repo.touched))
                .then_with(|| a.repo.name.cmp(&b.repo.name))
        });
        matches.truncate(limit);
        matches
    }

    /// A draft for `task` with the best matching repositories (at most [`DRAFT_LIMIT`]) and why each is there.
    /// Nothing runs until the draft is launched; the user can add and drop repositories first.
    pub fn draft(&self, task: &str) -> SpaceDraft {
        let mut draft = SpaceDraft::new(task);
        for found in self.search(task, DRAFT_LIMIT) {
            draft.add_repo(DraftRepo::new(found.repo.path.clone(), found.reason()));
        }
        draft
    }
}

/// Long words match on their first five letters, so "limiting" finds `limit.ts` and "payments" finds "payment".
fn stem(word: &str) -> String {
    word.chars().take(5).collect()
}

fn score_repo<'a>(repo: &'a IndexedRepo, words: &[String]) -> Option<RepoMatch<'a>> {
    const MAX_FILE_EXAMPLES: usize = 3;
    let name_words = task_words(&repo.name);
    let mut score = 0u32;
    let mut name_hits = Vec::new();
    let mut readme_hits = Vec::new();
    let mut file_examples: Vec<&str> = Vec::new();
    let mut recent_example: Option<&str> = None;
    for word in words {
        if name_words
            .iter()
            .any(|n| n.starts_with(word.as_str()) || word.starts_with(n.as_str()))
        {
            score += 8;
            name_hits.push(word.as_str());
        }
        let stem = stem(word);
        if repo.readme_lower.contains(&stem) {
            score += 2;
            readme_hits.push(word.as_str());
        }
        let hits: Vec<usize> = repo
            .files_lower
            .iter()
            .enumerate()
            .filter(|(_, f)| f.contains(&stem))
            .map(|(i, _)| i)
            .take(4)
            .collect();
        score += hits.len() as u32;
        for i in hits.into_iter().take(2) {
            let file = repo.files[i].as_str();
            if file_examples.len() < MAX_FILE_EXAMPLES && !file_examples.contains(&file) {
                file_examples.push(file);
            }
        }
        let recent = repo
            .recent_commits
            .iter()
            .chain(&repo.recent_branches)
            .find(|line| line.to_lowercase().contains(&stem));
        if let Some(line) = recent {
            score += 1;
            recent_example.get_or_insert(line.as_str());
        }
    }
    if score == 0 {
        return None;
    }
    let quoted = |ws: &[&str]| {
        ws.iter()
            .map(|w| format!("\"{w}\""))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut reasons = Vec::new();
    if !name_hits.is_empty() {
        reasons.push(format!("name matches {}", quoted(&name_hits)));
    }
    if !readme_hits.is_empty() {
        reasons.push(format!("README mentions {}", quoted(&readme_hits)));
    }
    if !file_examples.is_empty() {
        reasons.push(format!("files {}", file_examples.join(", ")));
    }
    if let Some(line) = recent_example {
        reasons.push(format!("recent work \"{line}\""));
    }
    Some(RepoMatch {
        repo,
        score,
        reasons,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_words_drop_short_and_common_words() {
        assert_eq!(
            task_words("Add rate-limiting to the Checkout API!"),
            vec!["rate", "limiting", "checkout", "api"]
        );
    }

    #[test]
    fn description_is_the_first_prose_paragraph() {
        let readme = "# Payments\n\n![badge](x)\n\nCharges carts\nand refunds them.\n\nMore.\n";
        assert_eq!(
            describe(readme).as_deref(),
            Some("Charges carts and refunds them.")
        );
        assert_eq!(
            describe("# Only a title\n").as_deref(),
            Some("Only a title")
        );
        assert_eq!(describe(""), None);
    }

    #[test]
    fn languages_rank_by_file_count() {
        let files: Vec<String> = ["a.rs", "b.rs", "c.ts", "d.md", "e.tsx", "f.rs", "g.py"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(languages(&files), vec!["Rust", "TypeScript", "Python"]);
    }
}
