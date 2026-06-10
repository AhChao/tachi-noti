use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
pub struct RepoInfo {
    pub name: String,
    pub branch: Option<String>,
}

/// Resolve project name and git branch by walking up from `cwd` and reading
/// `.git/HEAD` directly (no git subprocess). Any failure degrades gracefully.
pub fn detect(cwd: &Path) -> RepoInfo {
    let fallback_name = basename(cwd);
    let Some((toplevel, gitdir)) = find_gitdir(cwd) else {
        return RepoInfo { name: fallback_name, branch: None };
    };
    RepoInfo { name: basename(&toplevel), branch: read_branch(&gitdir) }
}

fn basename(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

/// Returns (repo toplevel, gitdir containing HEAD).
fn find_gitdir(start: &Path) -> Option<(PathBuf, PathBuf)> {
    for dir in start.ancestors() {
        let dotgit = dir.join(".git");
        let Ok(meta) = dotgit.symlink_metadata() else { continue };
        if meta.is_dir() {
            return Some((dir.to_path_buf(), dotgit));
        }
        if meta.is_file() {
            // Worktree/submodule: ".git" file contains "gitdir: <path>".
            let text = std::fs::read_to_string(&dotgit).ok()?;
            let raw = text.strip_prefix("gitdir:")?.trim();
            let gitdir = if Path::new(raw).is_absolute() {
                PathBuf::from(raw)
            } else {
                dir.join(raw)
            };
            return Some((dir.to_path_buf(), gitdir));
        }
    }
    None
}

fn read_branch(gitdir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(gitdir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(rest) = head.strip_prefix("ref: refs/heads/") {
        return Some(rest.to_string()); // branch names may contain '/', keep whole
    }
    if let Some(rest) = head.strip_prefix("ref: ") {
        return Some(rest.to_string()); // unusual ref, show as-is
    }
    if head.len() >= 7 && head.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(format!("{}\u{2026}", &head[..7])); // detached HEAD
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tachi-noti-test-gitinfo-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn normal_repo_with_branch() {
        let d = tmpdir("normal");
        let repo = d.join("myrepo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join(".git/HEAD"), "ref: refs/heads/feat/sub-branch\n").unwrap();
        let sub = repo.join("src/deep");
        fs::create_dir_all(&sub).unwrap();
        let info = detect(&sub);
        assert_eq!(info.name, "myrepo");
        assert_eq!(info.branch.as_deref(), Some("feat/sub-branch"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn detached_head() {
        let d = tmpdir("detached");
        let repo = d.join("r");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join(".git/HEAD"), "0123456789abcdef0123456789abcdef01234567\n").unwrap();
        let info = detect(&repo);
        assert_eq!(info.branch.as_deref(), Some("0123456\u{2026}"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn worktree_gitfile_relative() {
        let d = tmpdir("worktree");
        let main_gitdir = d.join("main/.git/worktrees/wt");
        fs::create_dir_all(&main_gitdir).unwrap();
        fs::write(main_gitdir.join("HEAD"), "ref: refs/heads/wt-branch\n").unwrap();
        let wt = d.join("wt");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".git"), "gitdir: ../main/.git/worktrees/wt\n").unwrap();
        let info = detect(&wt);
        assert_eq!(info.name, "wt");
        assert_eq!(info.branch.as_deref(), Some("wt-branch"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn not_a_repo() {
        let d = tmpdir("norepo");
        let sub = d.join("plain-folder");
        fs::create_dir_all(&sub).unwrap();
        let info = detect(&sub);
        assert_eq!(info.name, "plain-folder");
        assert_eq!(info.branch, None);
        let _ = fs::remove_dir_all(&d);
    }
}
