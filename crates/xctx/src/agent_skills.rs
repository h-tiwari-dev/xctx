//! The skills xctx gives every agent session it starts (agent panes and the Parent Agent): written at startup to
//! `<data dir>/agent-skills/<name>/SKILL.md`, with `omp-config.yml` beside them, an `omp --config` overlay that
//! lists the folder in `skills.customDirectories`. A runner loads them through its manifest's `skills` arguments
//! (see [`xctx_plugin::skills_args`]); only the built-in `omp` runner has them.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The folder in the data directory the skills are written to.
pub const SKILLS_FOLDER: &str = "agent-skills";

/// The `omp --config` overlay written beside the skills.
pub const OMP_CONFIG_FILE: &str = "omp-config.yml";

/// Each skill as `(name, SKILL.md text)`.
pub const SKILLS: &[(&str, &str)] = &[(
    "xctx-browser",
    include_str!("agent_skills/xctx-browser/SKILL.md"),
)];

/// The `omp --config` overlay that makes omp load the skills in `dir`.
pub fn omp_config(dir: &Path) -> String {
    // A JSON string is a YAML double-quoted scalar, so any path (spaces, quotes) survives.
    let dir = serde_json::Value::String(dir.to_string_lossy().into_owned());
    format!(
        "# Written by xctx at startup; edits are replaced.\nskills:\n  customDirectories:\n    - {dir}\n"
    )
}

/// Write [`SKILLS`] and the omp overlay to `<data_dir>/agent-skills`, replacing what an older xctx wrote, and
/// return that folder. A file that already holds the same text is left alone.
pub fn install(data_dir: &Path) -> io::Result<PathBuf> {
    let dir = data_dir.join(SKILLS_FOLDER);
    for (name, text) in SKILLS {
        write_if_changed(&dir.join(name).join("SKILL.md"), text)?;
    }
    write_if_changed(&dir.join(OMP_CONFIG_FILE), &omp_config(&dir))?;
    Ok(dir)
}

fn write_if_changed(path: &Path, text: &str) -> io::Result<()> {
    if fs::read_to_string(path).is_ok_and(|old| old == text) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn install_writes_each_skill_and_an_overlay_that_lists_the_folder() -> TestResult {
        let data = tempfile::tempdir()?;
        let data_dir = data.path().join("Application Support").join("xctx");
        let dir = install(&data_dir)?;
        assert_eq!(dir, data_dir.join(SKILLS_FOLDER));
        for (name, text) in SKILLS {
            assert_eq!(fs::read_to_string(dir.join(name).join("SKILL.md"))?, *text);
            assert!(
                text.starts_with(&format!("---\nname: {name}\ndescription: ")),
                "{name} has the frontmatter omp needs"
            );
        }
        let overlay = fs::read_to_string(dir.join(OMP_CONFIG_FILE))?;
        assert!(
            overlay.contains(&format!("    - \"{}\"\n", dir.display())),
            "the folder is quoted, spaces and all: {overlay}"
        );
        // Installing again over its own files changes nothing and does not fail.
        assert_eq!(install(&data_dir)?, dir);
        Ok(())
    }
}
