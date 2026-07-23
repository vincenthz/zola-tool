//! Thin wrapper over the `git` CLI: repo detection, per-file status, commit and
//! push. All functions are no-ops-friendly — if the blog is not a git repo the
//! caller simply skips them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::post::Result;

/// Git tracking state of a single file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GitState {
    /// Tracked and unmodified since the last commit.
    Tracked,
    /// Tracked but with staged or unstaged changes.
    Modified,
    /// Not tracked by git yet.
    Untracked,
}

/// Return the repository root containing `start`, or `None` if `start` is not
/// inside a git working tree (or git is unavailable).
pub fn repo_root(start: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!root.is_empty()).then(|| PathBuf::from(root))
}

/// Map of absolute path -> git state for every file that git reports as
/// untracked or modified. Files absent from the map are tracked and clean.
pub fn statuses(root: &Path) -> Result<HashMap<PathBuf, GitState>> {
    // `--untracked-files=all` lists individual files even inside a wholly
    // untracked directory, instead of collapsing it to the directory name.
    let out = run(root, &["status", "--porcelain", "--untracked-files=all"])?;
    let mut map = HashMap::new();
    for line in out.lines() {
        if line.len() < 4 {
            continue;
        }
        let code = &line[..2];
        let raw = line[3..].trim();
        // A rename is reported as "old -> new"; key on the current name.
        let rel = raw.rsplit(" -> ").next().unwrap_or(raw);
        let rel = rel.trim_matches('"');
        let state = if code == "??" {
            GitState::Untracked
        } else {
            GitState::Modified
        };
        map.insert(root.join(rel), state);
    }
    Ok(map)
}

/// Stage the given files and commit them with `message`.
pub fn add_and_commit(root: &Path, files: &[&Path], message: &str) -> Result<()> {
    let mut add: Vec<String> = vec!["add".into(), "--".into()];
    add.extend(files.iter().map(|f| f.to_string_lossy().into_owned()));
    let add_refs: Vec<&str> = add.iter().map(String::as_str).collect();
    run(root, &add_refs)?;
    run(root, &["commit", "-m", message])?;
    Ok(())
}

/// Push to the default remote, returning a short summary line on success.
pub fn push(root: &Path) -> Result<String> {
    let out = capture(root, &["push"])?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let last = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string();
    if out.status.success() {
        Ok(if last.is_empty() {
            "pushed".into()
        } else {
            last
        })
    } else {
        Err(if last.is_empty() {
            "git push failed".into()
        } else {
            last.into()
        })
    }
}

/// Run a git subcommand, returning stdout on success or an error with stderr.
fn run(root: &Path, args: &[&str]) -> Result<String> {
    let out = capture(root, args)?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let msg = String::from_utf8_lossy(&out.stderr);
        let msg = msg.trim();
        Err(format!(
            "git {}: {}",
            args.first().copied().unwrap_or(""),
            if msg.is_empty() { "failed" } else { msg }
        )
        .into())
    }
}

fn capture(root: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        // Never block the TUI waiting for interactive credentials.
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("could not run `git` ({e}); is it installed?").into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_repo() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "blog-tool-git-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        run(&dir, &["init", "-q"]).unwrap();
        // Local identity so commits work regardless of global config.
        run(&dir, &["config", "user.email", "test@example.com"]).unwrap();
        run(&dir, &["config", "user.name", "Test"]).unwrap();
        dir
    }

    #[test]
    fn repo_root_detects_repository() {
        let dir = temp_repo();
        let root = repo_root(&dir).expect("should be a repo");
        // Compare canonicalized paths (macOS /var -> /private/var symlink).
        assert_eq!(root.canonicalize().unwrap(), dir.canonicalize().unwrap());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn repo_root_is_none_outside_repo() {
        let dir = std::env::temp_dir().join(format!("blog-tool-nogit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert!(repo_root(&dir).is_none());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn statuses_classifies_files() {
        let dir = temp_repo();
        let file = dir.join("a.md");
        fs::write(&file, "one\n").unwrap();

        // Untracked.
        let st = statuses(&dir).unwrap();
        assert_eq!(st.get(&file), Some(&GitState::Untracked));

        // Tracked & clean after commit -> absent from the map.
        add_and_commit(&dir, &[&file], "add a").unwrap();
        assert!(!statuses(&dir).unwrap().contains_key(&file));

        // Modified after editing.
        fs::write(&file, "one\ntwo\n").unwrap();
        assert_eq!(
            statuses(&dir).unwrap().get(&file),
            Some(&GitState::Modified)
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn push_without_remote_errors_fast() {
        let dir = temp_repo();
        let file = dir.join("a.md");
        fs::write(&file, "x\n").unwrap();
        add_and_commit(&dir, &[&file], "c").unwrap();
        // No remote configured: must return an error (and not hang on a prompt).
        assert!(push(&dir).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
