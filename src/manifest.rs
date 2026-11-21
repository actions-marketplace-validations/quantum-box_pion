//! What a change to the workspace's root `Cargo.toml` touches.
//!
//! The root manifest changes often: every new crate edits `members`, every
//! shared dependency bump edits `[workspace.dependencies]`. Most of those
//! changes reach a few crates, so they are told apart from the ones that
//! change how everything builds.

use std::collections::BTreeSet;

use toml::Value;

/// How the root manifest at one revision differs from another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestChange {
    /// Only entries of `[workspace.dependencies]` differ, and possibly the
    /// member lists. Holds the package names of the entries that differ: the
    /// crates built from one of them are the ones the change reaches.
    ///
    /// A member list alone reaches nothing. A crate joins a build through a
    /// dependency on it, and that dependency is written in another crate's
    /// own manifest.
    Dependencies(BTreeSet<String>),
    /// Something else differs (a profile, a patch, the resolver), or one of
    /// the two files cannot be read.
    Everything,
}

/// Keys of `[workspace]` that list crates and decide nothing about a build.
const MEMBER_LISTS: [&str; 3] = ["members", "default-members", "exclude"];

impl ManifestChange {
    /// Compares the text of the root manifest at two revisions.
    pub fn between(base: &str, head: &str) -> Self {
        let (Ok(base), Ok(head)) = (base.parse::<toml::Table>(), head.parse::<toml::Table>())
        else {
            return Self::Everything;
        };
        let keys: BTreeSet<&String> = base.keys().chain(head.keys()).collect();
        for key in keys {
            if key != "workspace" && base.get(key) != head.get(key) {
                return Self::Everything;
            }
        }
        let empty = toml::Table::new();
        let table = |manifest: &toml::Table| match manifest.get("workspace") {
            Some(Value::Table(workspace)) => Some(workspace.clone()),
            None => Some(empty.clone()),
            Some(_) => None,
        };
        let (Some(base), Some(head)) = (table(&base), table(&head)) else {
            return Self::Everything;
        };
        let keys: BTreeSet<&String> = base.keys().chain(head.keys()).collect();
        for key in keys {
            let listed = MEMBER_LISTS.contains(&key.as_str());
            if key != "dependencies" && !listed && base.get(key) != head.get(key) {
                return Self::Everything;
            }
        }

        let dependencies = |workspace: &toml::Table| match workspace.get("dependencies") {
            Some(Value::Table(dependencies)) => Some(dependencies.clone()),
            None => Some(toml::Table::new()),
            Some(_) => None,
        };
        let (Some(base), Some(head)) = (dependencies(&base), dependencies(&head)) else {
            return Self::Everything;
        };
        let mut changed = BTreeSet::new();
        let keys: BTreeSet<&String> = base.keys().chain(head.keys()).collect();
        for key in keys {
            let (before, after) = (base.get(key), head.get(key));
            if before == after {
                continue;
            }
            // An entry can rename its package (`package = "..."`), and the
            // rename itself can be what changed.
            for entry in [before, after].into_iter().flatten() {
                changed.insert(package_name(key, entry));
            }
        }
        Self::Dependencies(changed)
    }
}

/// The package a `[workspace.dependencies]` entry resolves to.
fn package_name(key: &str, entry: &Value) -> String {
    match entry.get("package") {
        Some(Value::String(package)) => package.clone(),
        _ => key.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"
[workspace]
resolver = "2"
members = ["api", "core"]

[workspace.dependencies]
serde = "1.0.200"
core = { path = "core" }
http = { version = "1", package = "http-next" }

[profile.release]
lto = true
"#;

    fn names(change: ManifestChange) -> Vec<String> {
        match change {
            ManifestChange::Dependencies(names) => names.into_iter().collect(),
            ManifestChange::Everything => panic!("expected a dependency change"),
        }
    }

    #[test]
    fn a_new_member_reaches_nothing() {
        let head = BASE.replace(r#"["api", "core"]"#, r#"["api", "core", "tools"]"#);
        assert!(names(ManifestChange::between(BASE, &head)).is_empty());
    }

    #[test]
    fn a_changed_dependency_is_named() {
        let head = BASE.replace("1.0.200", "1.0.201");
        assert_eq!(names(ManifestChange::between(BASE, &head)), ["serde"]);
    }

    #[test]
    fn a_renamed_dependency_is_named_by_its_package() {
        let head = BASE.replace(r#"version = "1", package"#, r#"version = "2", package"#);
        assert_eq!(names(ManifestChange::between(BASE, &head)), ["http-next"]);
    }

    #[test]
    fn an_added_and_a_removed_dependency_are_named() {
        let head = BASE
            .replace("serde = \"1.0.200\"\n", "")
            .replace("core = {", "tokio = \"1\"\ncore = {");
        assert_eq!(
            names(ManifestChange::between(BASE, &head)),
            ["serde", "tokio"]
        );
    }

    #[test]
    fn a_profile_change_reaches_everything() {
        let head = BASE.replace("lto = true", "lto = false");
        assert_eq!(
            ManifestChange::between(BASE, &head),
            ManifestChange::Everything
        );
    }

    #[test]
    fn a_resolver_change_reaches_everything() {
        let head = BASE.replace(r#"resolver = "2""#, r#"resolver = "3""#);
        assert_eq!(
            ManifestChange::between(BASE, &head),
            ManifestChange::Everything
        );
    }

    #[test]
    fn a_file_that_does_not_parse_reaches_everything() {
        assert_eq!(
            ManifestChange::between(BASE, "[workspace"),
            ManifestChange::Everything
        );
    }
}
