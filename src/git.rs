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

/// Which revision's line numbers a change is expressed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The revision compared against. A deleted file still has lines here.
    Base,
    /// The revision planned for. A new file has lines here.
    Head,
}

/// One place a change touched, in the line numbers of one [`Side`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedLine {
    /// Zero-based line number.
    pub line: u32,
    /// When set, the change has no line of its own on this side: it sits
    /// between `line` and this line. That is where removed lines used to be
    /// on the head side, and where added lines will go on the base side.
    pub gap_to: Option<u32>,
    /// The text the change added here, when it added any.
    pub text: Option<String>,
    /// The hunk added lines and removed none.
    pub addition_only: bool,
}

/// The places each file changed, in the line numbers of `side`.
///
/// Files that do not exist on that side have no entry, and neither do files
/// git does not track yet.
pub fn changed_lines(
    repository: &Utf8Path,
    base: &Revision,
    head: Option<&Revision>,
    side: Side,
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
    parse_unified_diff(&run(repository, &diff)?, side)
}

/// One `@@ -a,b +c,d @@` block of a diff. Starts are one-based.
#[derive(Debug, Default)]
struct Hunk {
    old_start: u32,
    old_count: u32,
    new_start: u32,
    new_count: u32,
    added: Vec<String>,
}

impl Hunk {
    fn changed_lines(self, side: Side) -> Vec<ChangedLine> {
        let (start, count) = match side {
            Side::Base => (self.old_start, self.old_count),
            Side::Head => (self.new_start, self.new_count),
        };
        let addition_only = self.old_count == 0;
        if count == 0 {
            // Nothing of this hunk exists on this side. A zero-count start
            // names the line the change comes after.
            let text =
                (side == Side::Base && !self.added.is_empty()).then(|| self.added.join("\n"));
            return vec![ChangedLine {
                line: start.saturating_sub(1),
                gap_to: Some(start),
                text,
                addition_only,
            }];
        }
        let mut added = self.added.into_iter();
        (0..count)
            .map(|offset| ChangedLine {
                line: start.saturating_sub(1) + offset,
                gap_to: None,
                text: match side {
                    Side::Head => added.next(),
                    Side::Base => None,
                },
                addition_only,
            })
            .collect()
    }
}

fn parse_unified_diff(
    diff: &str,
    side: Side,
) -> Result<BTreeMap<RepoPath, Vec<ChangedLine>>, GitError> {
    let mut files: BTreeMap<RepoPath, Vec<ChangedLine>> = BTreeMap::new();
    // The file on `side` the current hunks belong to.
    let mut current: Option<RepoPath> = None;
    let mut hunk: Option<Hunk> = None;
    let mut close = |hunk: &mut Option<Hunk>, current: &Option<RepoPath>| {
        if let (Some(hunk), Some(path)) = (hunk.take(), current) {
            files
                .entry(path.clone())
                .or_default()
                .extend(hunk.changed_lines(side));
        }
    };
    let (marker, prefix) = match side {
        Side::Base => ("--- ", "a/"),
        Side::Head => ("+++ ", "b/"),
    };
    let mut in_header = false;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            close(&mut hunk, &current);
            current = None;
            in_header = true;
            continue;
        }
        if in_header {
            if let Some(path) = line.strip_prefix(marker) {
                // `/dev/null` has no prefix: the file is absent on this side.
                current = match path.strip_prefix(prefix) {
                    Some(path) => Some(RepoPath::parse(path)?),
                    None => None,
                };
            }
            if !line.starts_with("@@ ") {
                continue;
            }
            in_header = false;
        }
        if let Some(header) = line.strip_prefix("@@ ") {
            close(&mut hunk, &current);
            hunk = parse_hunk_header(header);
        } else if let (Some(hunk), Some(text)) = (&mut hunk, line.strip_prefix('+')) {
            hunk.added.push(text.to_owned());
        }
    }
    close(&mut hunk, &current);
    Ok(files)
}

/// Reads `-a,b +c,d @@ …`. A missing count is 1.
fn parse_hunk_header(header: &str) -> Option<Hunk> {
    let mut parts = header.split(' ');
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let span = |text: &str| -> Option<(u32, u32)> {
        match text.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((text.parse().ok()?, 1)),
        }
    };
    let (old_start, old_count) = span(old)?;
    let (new_start, new_count) = span(new)?;
    Some(Hunk {
        old_start,
        old_count,
        new_start,
        new_count,
        added: Vec::new(),
    })
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

    const DIFF: &str = "\
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

    fn line(
        line: u32,
        gap_to: Option<u32>,
        text: Option<&str>,
        addition_only: bool,
    ) -> ChangedLine {
        ChangedLine {
            line,
            gap_to,
            text: text.map(str::to_owned),
            addition_only,
        }
    }

    #[test]
    fn the_head_side_lists_added_lines_and_the_gap_a_deletion_left() {
        let files = parse_unified_diff(DIFF, Side::Head).expect("valid diff");

        // `gone.rs` does not exist in the head revision.
        assert_eq!(files.len(), 1);
        assert_eq!(
            files[&RepoPath::parse("src/lib.rs").unwrap()],
            [
                line(3, None, Some("    let a = 1;"), true),
                line(4, None, Some("    let b = 2;"), true),
                line(11, None, Some("new"), false),
                line(20, Some(21), None, false),
            ]
        );
    }

    #[test]
    fn the_base_side_lists_removed_lines_and_the_gap_an_addition_fills() {
        let files = parse_unified_diff(DIFF, Side::Base).expect("valid diff");

        assert_eq!(
            files[&RepoPath::parse("src/lib.rs").unwrap()],
            [
                line(2, Some(3), Some("    let a = 1;\n    let b = 2;"), true),
                line(9, None, None, false),
                line(10, None, None, false),
                line(19, None, None, false),
            ]
        );
        // The deleted file still has its line in the base revision.
        assert_eq!(
            files[&RepoPath::parse("gone.rs").unwrap()],
            [line(0, None, None, false)]
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
