//! Reads the changed paths from git.

use std::process::Command;

use camino::{Utf8Path, Utf8PathBuf};

use crate::change::{RepoPath, RepoPathError};

/// A git revision (branch, tag or commit) given on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision(String);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RevisionError {
    #[error("revision is empty")]
    Empty,
    #[error("revision `{0}` looks like a command-line option")]
    LooksLikeOption(String),
}

impl Revision {
    /// Rejects values git would read as an option instead of a revision.
    pub fn parse(raw: &str) -> Result<Self, RevisionError> {
        if raw.is_empty() {
            return Err(RevisionError::Empty);
        }
        if raw.starts_with('-') {
            return Err(RevisionError::LooksLikeOption(raw.to_owned()));
        }
        Ok(Self(raw.to_owned()))
    }
}

impl std::str::FromStr for Revision {
    type Err = RevisionError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("could not run git: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("`git {command}` failed: {stderr}")]
    Failed { command: String, stderr: String },
    #[error("git printed a path that is not valid UTF-8")]
    NotUtf8,
    #[error(transparent)]
    Path(#[from] RepoPathError),
}

/// The root directory of the repository that contains `dir`.
pub fn repository_root(dir: &Utf8Path) -> Result<Utf8PathBuf, GitError> {
    let output = run(dir, &["rev-parse", "--show-toplevel"])?;
    Ok(Utf8PathBuf::from(output.trim_end_matches('\n')))
}

/// The paths that differ between `base` and `head`.
///
/// Without `head` the comparison is against the working tree, and files git
/// does not track yet are included. A rename counts as both of its paths.
pub fn changed_paths(
    repository: &Utf8Path,
    base: &Revision,
    head: Option<&Revision>,
) -> Result<Vec<RepoPath>, GitError> {
    let mut diff = vec!["diff", "--name-only", "-z", "--no-renames", &base.0];
    if let Some(head) = head {
        diff.push(&head.0);
    }
    diff.push("--");
    let mut listed = run(repository, &diff)?;
    if head.is_none() {
        let untracked = ["ls-files", "-z", "--others", "--exclude-standard"];
        listed.push_str(&run(repository, &untracked)?);
    }
    listed
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(|path| RepoPath::parse(path).map_err(GitError::from))
        .collect()
}

fn run(dir: &Utf8Path, args: &[&str]) -> Result<String, GitError> {
    let output = Command::new("git").arg("-C").arg(dir).args(args).output()?;
    if !output.status.success() {
        return Err(GitError::Failed {
            command: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    String::from_utf8(output.stdout).map_err(|_| GitError::NotUtf8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_rejects_option_like_values() {
        assert_eq!(Revision::parse(""), Err(RevisionError::Empty));
        assert_eq!(
            Revision::parse("--output=/tmp/x"),
            Err(RevisionError::LooksLikeOption("--output=/tmp/x".into()))
        );
        assert!(Revision::parse("origin/main").is_ok());
    }
}
