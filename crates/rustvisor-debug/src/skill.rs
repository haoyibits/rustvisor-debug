//! `rvdbg skill`: install the agent skill that says which rvdbg command does
//! what, as `<skills>/rvdbg/SKILL.md` (Claude Code's layout).

use std::path::{Path, PathBuf};

const TEMPLATE: &str = include_str!("../../../skills/rvdbg/SKILL.md");

/// The skill with this rvdbg's version in its frontmatter.
pub fn render() -> String {
    TEMPLATE.replace("{version}", env!("CARGO_PKG_VERSION"))
}

/// `$CLAUDE_CONFIG_DIR/skills`, else `~/.claude/skills`.
fn default_dir() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir).join("skills"));
    }
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".claude").join("skills"))
        .ok_or_else(|| "HOME is not set; pass --dir".to_string())
}

/// Write the skill into `dir` (or the default skills directory). A changed
/// existing file is kept as `SKILL.md.bak`.
pub fn install(dir: Option<&Path>) -> Result<PathBuf, String> {
    let skills = match dir {
        Some(dir) => dir.to_path_buf(),
        None => default_dir()?,
    };
    write(&skills)
}

fn write(skills: &Path) -> Result<PathBuf, String> {
    let folder = skills.join("rvdbg");
    let path = folder.join("SKILL.md");
    let text = render();
    std::fs::create_dir_all(&folder)
        .map_err(|error| format!("cannot create {}: {error}", folder.display()))?;
    if let Ok(existing) = std::fs::read_to_string(&path) {
        if existing == text {
            return Ok(path);
        }
        let backup = folder.join("SKILL.md.bak");
        std::fs::copy(&path, &backup)
            .map_err(|error| format!("cannot back up {}: {error}", path.display()))?;
    }
    std::fs::write(&path, text)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_skill_names_itself_and_carries_the_version() {
        let text = render();
        assert!(text.starts_with("---\nname: rvdbg\n"));
        assert!(text.contains(&format!("version: \"{}\"", env!("CARGO_PKG_VERSION"))));
        assert!(!text.contains("{version}"));
    }

    #[test]
    fn install_is_idempotent_and_keeps_a_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path()).unwrap();
        assert_eq!(path, dir.path().join("rvdbg/SKILL.md"));
        write(dir.path()).unwrap();
        assert!(!dir.path().join("rvdbg/SKILL.md.bak").exists());
        std::fs::write(&path, "edited").unwrap();
        write(dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("rvdbg/SKILL.md.bak")).unwrap(),
            "edited"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), render());
    }
}
