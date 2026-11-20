//! Reads the changed paths from git.

use std::collections::BTreeMap;
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

/// One line of the head revision that a change touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedLine {
    /// Zero-based line number in the head revision.
    pub line: u32,
    /// The line's text when the change wrote it; `None` for a line that only
    /// sits next to a deletion.
    pub text: Option<String>,
    /// The hunk added lines and removed none.
    pub addition_only: bool,
}

/// The changed lines of each file that still exists in the head revision.
///
/// A deletion has no line of its own in the head revision, so the lines on
/// both sides of it are reported instead. Files git does not track yet have
/// no entry.
pub fn changed_lines(
    repository: &Utf8Path,
    base: &Revision,
    head: Option<&Revision>,
) -> Result<BTreeMap<RepoPath, Vec<ChangedLine>>, GitError> {
    let mut diff = vec![
        "-c",
        "core.quotePath=false",
        "diff",
        "--unified=0",
        "--no-renames",
        "--no-color",
        "--no-ext-diff",
        &base.0,
    ];
    if let Some(head) = head {
        diff.push(&head.0);
    }
    diff.push("--");
    parse_unified_diff(&run(repository, &diff)?)
}

fn parse_unified_diff(diff: &str) -> Result<BTreeMap<RepoPath, Vec<ChangedLine>>, GitError> {
    let mut files: BTreeMap<RepoPath, Vec<ChangedLine>> = BTreeMap::new();
    let mut current: Option<RepoPath> = None;
    // The next added line's number and whether its hunk only adds.
    let mut next_line = 0;
    let mut addition_only = false;
    let mut in_hunk = false;
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ ") {
            in_hunk = false;
            current = match path.strip_prefix("b/") {
                Some(path) => Some(RepoPath::parse(path)?),
                // `+++ /dev/null`: the file was deleted.
                None => None,
            };
            if let Some(path) = &current {
                files.entry(path.clone()).or_default();
            }
            continue;
        }
        if line.starts_with("diff --git ") {
            in_hunk = false;
            current = None;
            continue;
        }
        let Some(path) = &current else { continue };
        let lines = files.entry(path.clone()).or_default();
        if let Some(header) = line.strip_prefix("@@ ") {
            let Some((removed, start, added)) = parse_hunk_header(header) else {
                continue;
            };
            in_hunk = true;
            addition_only = removed == 0;
            // Hunk line numbers are one-based.
            next_line = start.saturating_sub(1);
            if added == 0 {
                // Lines were only removed, after head line `start`.
                for line in [start.saturating_sub(1), start] {
                    lines.push(ChangedLine {
                        line,
                        text: None,
                        addition_only: false,
                    });
                }
            }
            continue;
        }
        if !in_hunk {
            continue;
        }
        if let Some(text) = line.strip_prefix('+') {
            lines.push(ChangedLine {
                line: next_line,
                text: Some(text.to_owned()),
                addition_only,
            });
            next_line += 1;
        }
    }
    Ok(files)
}

/// Reads `-a,b +c,d @@ …` as `(b, c, d)`. A missing count is 1.
fn parse_hunk_header(header: &str) -> Option<(u32, u32, u32)> {
    let mut parts = header.split(' ');
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let span = |text: &str| -> Option<(u32, u32)> {
        match text.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((text.parse().ok()?, 1)),
        }
    };
    let (_, removed) = span(old)?;
    let (start, added) = span(new)?;
    Some((removed, start, added))
}

/// The text of `path` at `revision`, or `None` when it does not exist there.
pub fn file_at(
    repository: &Utf8Path,
    revision: &Revision,
    path: &RepoPath,
) -> Result<Option<String>, GitError> {
    let object = format!("{}:{path}", revision.0);
    match run(repository, &["show", &object]) {
        Ok(text) => Ok(Some(text)),
        Err(GitError::Failed { .. }) => Ok(None),
        Err(error) => Err(error),
    }
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
    fn a_diff_lists_added_lines_and_the_lines_around_a_deletion() {
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
index 111..222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -3,0 +4,2 @@ fn before()
+    let a = 1;
+    let b = 2;
@@ -10,2 +12 @@
-old one
-old two
+new
@@ -20 +21,0 @@
-removed
diff --git a/gone.rs b/gone.rs
deleted file mode 100644
--- a/gone.rs
+++ /dev/null
@@ -1 +0,0 @@
-fn gone() {}
";
        let files = parse_unified_diff(diff).expect("valid diff");
        let line = |line, text: Option<&str>, addition_only| ChangedLine {
            line,
            text: text.map(str::to_owned),
            addition_only,
        };

        assert_eq!(files.len(), 1);
        assert_eq!(
            files[&RepoPath::parse("src/lib.rs").unwrap()],
            [
                line(3, Some("    let a = 1;"), true),
                line(4, Some("    let b = 2;"), true),
                line(11, Some("new"), false),
                line(20, None, false),
                line(21, None, false),
            ]
        );
    }

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
