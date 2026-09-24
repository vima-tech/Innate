//! Project identity: which project a piece of work happened in.
//!
//! A rule only counts as transferable knowledge once it has been validated in a
//! project other than the ones it was born from (living-knowledge redesign,
//! schema 5.0). That needs a stable name for "the project", and before 5.0 the
//! library recorded none at all — hooks read `cwd` only to build a query string.
//!
//! The project is the name of the nearest ancestor directory holding `.git`
//! (a directory for a normal checkout, a file for a worktree or submodule), so
//! any subdirectory of a repository maps to the same project. Outside a
//! repository the working directory's own name is used. The home directory and
//! the filesystem root are not projects.

use std::path::{Path, PathBuf};

/// Project name for `dir`, or `None` when `dir` is the home directory, the root,
/// or has no usable name.
pub fn project_of(dir: &Path) -> Option<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let root = dir
        .ancestors()
        .find(|d| d.join(".git").exists())
        .unwrap_or(dir);
    if root.parent().is_none() || home.as_deref() == Some(root) {
        return None;
    }
    root.file_name()
        .map(|n| n.to_string_lossy().trim().to_string())
        .filter(|n| !n.is_empty())
}

/// Project of the current process's working directory. Hooks, the MCP server and
/// CLI commands are all started by the agent inside the project it works on.
pub fn current_project() -> Option<String> {
    std::env::current_dir().ok().as_deref().and_then(project_of)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subdirectory_of_a_repository_maps_to_the_repository() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("shop-api");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src/handlers")).unwrap();
        assert_eq!(
            project_of(&repo.join("src/handlers")).as_deref(),
            Some("shop-api")
        );
        assert_eq!(project_of(&repo).as_deref(), Some("shop-api"));
    }

    #[test]
    fn worktree_git_file_counts_as_a_repository_root() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("feature-x");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: /elsewhere").unwrap();
        assert_eq!(project_of(&wt).as_deref(), Some("feature-x"));
    }

    #[test]
    fn plain_directory_uses_its_own_name_and_root_is_not_a_project() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("notes");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(project_of(&dir).as_deref(), Some("notes"));
        assert_eq!(project_of(Path::new("/")), None);
    }
}
