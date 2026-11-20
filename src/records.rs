//! Reads the reference records `pion-rustc` writes while a workspace
//! compiles: one JSON file per crate and kind of build.
//!
//! The compiler has already resolved every name and method call when it
//! writes them, including calls into code a macro generated, so nothing is
//! inferred here.

use camino::Utf8Path;
use serde::Deserialize;

/// What one compiled crate defines and refers to.
#[derive(Debug, Clone, Deserialize)]
pub struct CrateRecords {
    /// The Cargo package the crate belongs to.
    pub package: String,
    pub definitions: Vec<Definition>,
    pub mentions: Vec<Mention>,
    /// `(changed, affected)` pairs of definition ids.
    pub affects: Vec<(String, String)>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Definition {
    pub id: String,
    /// The last path segment.
    pub name: String,
    /// A function or method.
    pub callable: bool,
    /// `module::path::name` when the definition is a test.
    pub test: Option<String>,
    /// Path relative to the workspace root. Absent for code a macro
    /// generated, which owns no line of any file.
    pub file: Option<String>,
    /// One-based, attributes and doc comments included.
    pub first_line: Option<u32>,
    pub last_line: Option<u32>,
}

/// A place where a definition is mentioned.
#[derive(Debug, Clone, Deserialize)]
pub struct Mention {
    pub file: String,
    /// One-based.
    pub line: u32,
    /// Zero-based.
    pub column: u32,
    pub to: String,
    pub callable: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordsError {
    #[error("could not read reference records: {0}")]
    Io(#[from] std::io::Error),
    #[error("`{path}` is not a reference record: {source}")]
    Json {
        path: String,
        source: serde_json::Error,
    },
}

/// Reads every `*.json` record in `directory`.
pub fn read_directory(directory: &Utf8Path) -> Result<Vec<CrateRecords>, RecordsError> {
    let mut paths = Vec::new();
    for entry in directory.read_dir_utf8()? {
        let path = entry?.into_path();
        if path.extension() == Some("json") {
            paths.push(path);
        }
    }
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path)?;
            serde_json::from_str(&text).map_err(|source| RecordsError::Json {
                path: path.into_string(),
                source,
            })
        })
        .collect()
}
