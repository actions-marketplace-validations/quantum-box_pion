//! Which external packages a `Cargo.lock` change touches.

use std::collections::{BTreeMap, BTreeSet};

/// One `[[package]]` entry of a lockfile.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LockedPackage {
    pub name: String,
    pub version: String,
    /// Where the package comes from. Absent for workspace members.
    pub source: Option<String>,
    pub checksum: Option<String>,
}

/// The parts of a lockfile that decide what gets built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lockfile {
    /// The `version = N` line at the top: the format of the file itself.
    format: Option<String>,
    packages: BTreeSet<LockedPackage>,
    /// Each package's `dependencies` list, as cargo writes the entries:
    /// `name`, `name version` or `name version (source)`.
    dependencies: BTreeMap<LockedPackage, Vec<String>>,
}

impl Lockfile {
    /// Reads the format cargo writes. Lines pion does not need (dependency
    /// lists, comments, metadata) are skipped.
    pub fn parse(text: &str) -> Self {
        let mut format = None;
        let mut packages = BTreeSet::new();
        let mut dependencies = BTreeMap::new();
        let mut current: Option<(LockedPackage, Vec<String>)> = None;
        let mut in_package = false;
        let mut in_dependencies = false;
        let mut finish = |entry: Option<(LockedPackage, Vec<String>)>| {
            if let Some((package, needs)) = entry {
                dependencies.insert(package.clone(), needs);
                packages.insert(package);
            }
        };
        for line in text.lines() {
            let line = line.trim();
            if in_dependencies {
                match (&mut current, line) {
                    (_, "]") => in_dependencies = false,
                    (Some((_, needs)), entry) => {
                        needs.push(entry.trim_end_matches(',').trim_matches('"').to_owned());
                    }
                    (None, _) => {}
                }
                continue;
            }
            if line.starts_with('[') {
                finish(current.take());
                in_package = line == "[[package]]";
                if in_package {
                    let package = LockedPackage {
                        name: String::new(),
                        version: String::new(),
                        source: None,
                        checksum: None,
                    };
                    current = Some((package, Vec::new()));
                }
                continue;
            }
            let Some((key, value)) = line.split_once(" = ") else {
                continue;
            };
            if key == "dependencies" && value == "[" {
                in_dependencies = true;
                continue;
            }
            let value = value.trim_matches('"').to_owned();
            match (&mut current, key) {
                (Some((package, _)), "name") => package.name = value,
                (Some((package, _)), "version") => package.version = value,
                (Some((package, _)), "source") => package.source = Some(value),
                (Some((package, _)), "checksum") => package.checksum = Some(value),
                (None, "version") if !in_package => format = Some(value),
                _ => {}
            }
        }
        finish(current);
        Self {
            format,
            packages,
            dependencies,
        }
    }

    /// The external packages the workspace member `member` is resolved with:
    /// everything its `dependencies` lead to, through members and external
    /// packages alike. `None` when the lockfile has no such member.
    ///
    /// A lockfile does not tell dev-dependencies apart, so they are included.
    pub fn externals_of(&self, member: &str) -> Option<BTreeSet<LockedPackage>> {
        let root = self
            .packages
            .iter()
            .find(|package| package.name == member && package.source.is_none())?;
        let mut seen = BTreeSet::from([root]);
        let mut pending = vec![root];
        while let Some(package) = pending.pop() {
            for entry in self.dependencies.get(package).into_iter().flatten() {
                for dependency in self.resolve(entry) {
                    if seen.insert(dependency) {
                        pending.push(dependency);
                    }
                }
            }
        }
        Some(
            seen.into_iter()
                .filter(|package| package.source.is_some())
                .cloned()
                .collect(),
        )
    }

    /// The packages a `dependencies` entry names. Cargo adds the version, and
    /// then the source, only when the name alone is ambiguous.
    fn resolve<'a>(&'a self, entry: &'a str) -> impl Iterator<Item = &'a LockedPackage> {
        let mut parts = entry.splitn(3, ' ');
        let name = parts.next().unwrap_or_default();
        let version = parts.next();
        let source = parts
            .next()
            .map(|source| source.trim_start_matches('(').trim_end_matches(')'));
        self.packages.iter().filter(move |package| {
            package.name == name
                && version.is_none_or(|version| package.version == version)
                && source.is_none_or(|source| package.source.as_deref() == Some(source))
        })
    }

    /// Whether the format line differs: the two files cannot be compared.
    pub fn format_differs(&self, base: &Self) -> bool {
        self.format != base.format
    }

    /// What `self` has that `base` does not.
    pub fn changes_since(&self, base: &Self) -> LockChange {
        if self.format != base.format {
            return LockChange::Everything;
        }
        let packages = self
            .packages
            .difference(&base.packages)
            // A workspace member's entry changes with its own Cargo.toml,
            // which is a changed file in its own right.
            .filter(|package| package.source.is_some())
            .cloned()
            .collect();
        LockChange::Packages(packages)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockChange {
    /// The external packages that are new or resolved differently. A package
    /// that only disappeared is not listed: nothing is built from it anymore.
    Packages(Vec<LockedPackage>),
    /// The lockfile format changed, so the two files cannot be compared.
    Everything,
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"
# This file is automatically @generated by Cargo.
version = 4

[[package]]
name = "app"
version = "0.1.0"
dependencies = [
 "serde",
]

[[package]]
name = "serde"
version = "1.0.200"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "aaa"
"#;

    #[test]
    fn an_upgraded_package_is_the_only_change() {
        let head = BASE
            .replace("1.0.200", "1.0.201")
            .replace("aaa", "bbb")
            .replace("0.1.0", "0.1.1");
        let change = Lockfile::parse(&head).changes_since(&Lockfile::parse(BASE));

        assert_eq!(
            change,
            LockChange::Packages(vec![LockedPackage {
                name: "serde".into(),
                version: "1.0.201".into(),
                source: Some("registry+https://github.com/rust-lang/crates.io-index".into()),
                checksum: Some("bbb".into()),
            }])
        );
    }

    #[test]
    fn a_format_change_cannot_be_narrowed() {
        let head = BASE.replace("version = 4", "version = 5");
        let change = Lockfile::parse(&head).changes_since(&Lockfile::parse(BASE));

        assert_eq!(change, LockChange::Everything);
    }

    #[test]
    fn an_identical_lockfile_changes_nothing() {
        let change = Lockfile::parse(BASE).changes_since(&Lockfile::parse(BASE));

        assert_eq!(change, LockChange::Packages(vec![]));
    }

    const TWO_MEMBERS: &str = r#"
version = 4

[[package]]
name = "api"
version = "0.1.0"
dependencies = [
 "core",
 "http 1.0.0",
]

[[package]]
name = "core"
version = "0.1.0"
dependencies = [
 "serde",
]

[[package]]
name = "worker"
version = "0.1.0"
dependencies = [
 "http 0.2.0",
]

[[package]]
name = "http"
version = "0.2.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "h02"

[[package]]
name = "http"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "h10"

[[package]]
name = "serde"
version = "1.0.200"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "aaa"
"#;

    fn externals(lock: &str, member: &str) -> Vec<String> {
        Lockfile::parse(lock)
            .externals_of(member)
            .expect("member is in the lockfile")
            .into_iter()
            .map(|package| format!("{} {}", package.name, package.version))
            .collect()
    }

    #[test]
    fn a_member_is_resolved_with_what_its_dependencies_lead_to() {
        assert_eq!(
            externals(TWO_MEMBERS, "api"),
            ["http 1.0.0", "serde 1.0.200"]
        );
        assert_eq!(externals(TWO_MEMBERS, "worker"), ["http 0.2.0"]);
    }

    #[test]
    fn an_unknown_member_has_no_answer() {
        assert_eq!(Lockfile::parse(TWO_MEMBERS).externals_of("nobody"), None);
        // An external package is not a member.
        assert_eq!(Lockfile::parse(TWO_MEMBERS).externals_of("serde"), None);
    }
}
