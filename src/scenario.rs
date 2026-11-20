//! API scenario files and the handlers they exercise.
//!
//! Scenarios are the YAML (or Markdown with YAML blocks) files muon runs:
//! a list of steps, each with a `request:` that has a `method:` and a `url:`.
//! They are read line by line rather than with a YAML parser: only those two
//! keys and the `query:` block of a GraphQL request matter here.

use std::collections::BTreeSet;

use camino::{Utf8Path, Utf8PathBuf};

use crate::change::{RepoPath, RepoPathError};
use crate::routes::{PathPattern, RouteTable};
use crate::symbols::{ReferenceGraph, SymbolId};
use crate::workspace::{PackageName, WorkspaceGraph};

/// One HTTP request a scenario makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub url: String,
    /// The top-level fields of the GraphQL operations in the request body.
    pub graphql_fields: Vec<String>,
}

/// What a scenario file asks of the API.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScenarioRequests {
    pub requests: Vec<Request>,
    /// A `request:` whose method or URL could not be read.
    pub unreadable: bool,
}

impl ScenarioRequests {
    pub fn read(text: &str) -> Self {
        let mut read = Self::default();
        let mut method: Option<String> = None;
        let mut url: Option<String> = None;
        let mut fields = Vec::new();
        // The column of the open `request:` key, and of its direct children.
        let mut request_column: Option<usize> = None;
        let mut child_column: Option<usize> = None;
        let mut lines = text.lines().peekable();

        while let Some(line) = lines.next() {
            if line.trim().is_empty() {
                continue;
            }
            let (column, entry) = key_column(line);
            if request_column.is_some_and(|request| column <= request) {
                read.close(&mut request_column, &mut method, &mut url, &mut fields);
                child_column = None;
            }
            if entry == "request:" {
                read.close(&mut request_column, &mut method, &mut url, &mut fields);
                request_column = Some(column);
                child_column = None;
                continue;
            }
            if request_column.is_none() {
                continue;
            }
            // `method:` and `url:` count only as direct children of
            // `request:`; a body may have keys of the same name.
            let is_child = *child_column.get_or_insert(column) == column;
            if let Some(value) = entry.strip_prefix("method:").filter(|_| is_child) {
                method = Some(unquote(value).to_uppercase());
            } else if let Some(value) = entry.strip_prefix("url:").filter(|_| is_child) {
                url = Some(unquote(value).to_owned());
            } else if let Some(value) = entry.strip_prefix("query:") {
                let value = value.trim();
                let query = if value.starts_with('|') || value.starts_with('>') {
                    // A block scalar: the lines indented deeper than the key.
                    let mut block = String::new();
                    while let Some(next) =
                        lines.next_if(|next| next.trim().is_empty() || indentation(next) > column)
                    {
                        block.push_str(next);
                        block.push('\n');
                    }
                    block
                } else {
                    unquote(value).to_owned()
                };
                fields.extend(graphql_top_level_fields(&query));
            }
        }
        read.close(&mut request_column, &mut method, &mut url, &mut fields);
        read
    }

    fn close(
        &mut self,
        request_column: &mut Option<usize>,
        method: &mut Option<String>,
        url: &mut Option<String>,
        fields: &mut Vec<String>,
    ) {
        if request_column.take().is_none() {
            return;
        }
        match (method.take(), url.take()) {
            (Some(method), Some(url)) => self.requests.push(Request {
                method,
                url,
                graphql_fields: std::mem::take(fields),
            }),
            _ => {
                fields.clear();
                self.unreadable = true;
            }
        }
    }
}

/// The column a line's key starts at, and the line from that key on. A list
/// item's key starts after its `- `.
fn key_column(line: &str) -> (usize, &str) {
    let mut column = indentation(line);
    let mut entry = line.trim_start();
    while let Some(rest) = entry.strip_prefix("- ") {
        column += entry.len() - rest.trim_start().len();
        entry = rest.trim_start();
    }
    (column, entry.trim_end())
}

fn indentation(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn unquote(value: &str) -> &str {
    let value = value.trim();
    let value = value.split(" #").next().unwrap_or(value).trim();
    value.trim_matches(['"', '\''])
}

/// The fields selected at the top level of each operation in a GraphQL
/// document: `createFeature` in `mutation M { createFeature(input: $i) { id } }`.
fn graphql_top_level_fields(query: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut braces = 0usize;
    let mut parens = 0usize;
    let mut chars = query.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        match c {
            '{' => braces += 1,
            '}' => braces = braces.saturating_sub(1),
            '(' => parens += 1,
            ')' => parens = parens.saturating_sub(1),
            '#' => {
                // A comment runs to the end of the line.
                while chars.next_if(|(_, c)| *c != '\n').is_some() {}
            }
            '"' => {
                while let Some((_, c)) = chars.next() {
                    match c {
                        '\\' => {
                            chars.next();
                        }
                        '"' => break,
                        _ => {}
                    }
                }
            }
            c if (c.is_alphabetic() || c == '_') && braces == 1 && parens == 0 => {
                let mut end = start + c.len_utf8();
                while let Some((index, c)) =
                    chars.next_if(|(_, c)| c.is_alphanumeric() || *c == '_')
                {
                    end = index + c.len_utf8();
                }
                let rest = query[end..].trim_start();
                // `alias: field` names the field second.
                if !rest.starts_with(':') {
                    fields.push(query[start..end].to_owned());
                }
            }
            _ => {}
        }
    }
    fields
}

/// `createFeature` -> `create_feature`, the name of the resolver method.
fn snake_case(name: &str) -> String {
    let mut snake = String::new();
    for c in name.chars() {
        if c.is_uppercase() {
            if !snake.is_empty() {
                snake.push('_');
            }
            snake.extend(c.to_lowercase());
        } else {
            snake.push(c);
        }
    }
    snake
}

/// A scenario file tied to the code it exercises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scenario {
    pub path: RepoPath,
    /// The crate whose directory holds the file: the API under test.
    pub package: Option<PackageName>,
    /// The handlers and resolvers its requests reach.
    pub handlers: BTreeSet<SymbolId>,
    /// At least one request could not be tied to a handler. Such a scenario
    /// is selected whenever its crate is affected at all.
    pub untraced: bool,
}

impl Scenario {
    /// Resolves each request of a scenario to handlers.
    ///
    /// A REST request is matched against the route table. A GraphQL request
    /// is matched by name: a top-level field `createFeature` is taken to be
    /// served by every method called `create_feature`. That is a guess about
    /// naming, so it can select too much, and a field with no such method
    /// makes the scenario untraced.
    pub fn resolve(
        path: RepoPath,
        package: Option<PackageName>,
        requests: &ScenarioRequests,
        routes: &RouteTable,
        symbols: &ReferenceGraph,
    ) -> Self {
        let mut handlers = BTreeSet::new();
        let mut untraced = requests.unreadable || requests.requests.is_empty();
        for request in &requests.requests {
            let found: BTreeSet<SymbolId> = if request.graphql_fields.is_empty() {
                routes
                    .handlers(&request.method, &PathPattern::request(&request.url))
                    .into_iter()
                    .cloned()
                    .collect()
            } else {
                let mut resolvers = BTreeSet::new();
                for field in &request.graphql_fields {
                    let named: Vec<&SymbolId> =
                        symbols.callables_named(&snake_case(field)).collect();
                    untraced |= named.is_empty();
                    resolvers.extend(named.into_iter().cloned());
                }
                resolvers
            };
            untraced |= found.is_empty();
            handlers.extend(found);
        }
        Self {
            path,
            package,
            handlers,
            untraced,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScenarioError {
    #[error("could not read scenarios: {0}")]
    Io(#[from] std::io::Error),
    #[error("`{0}` is outside the repository")]
    OutsideRepository(Utf8PathBuf),
    #[error(transparent)]
    Path(#[from] RepoPathError),
}

/// File name endings of scenario files.
const SCENARIO_SUFFIXES: [&str; 3] = [".yaml", ".yml", ".scenario.md"];

/// Reads every scenario file under `directories` and ties its requests to
/// handlers. Returns the scenarios and the directories as repository paths.
///
/// Without a reference graph no request can be tied to a handler, so every
/// scenario is untraced.
pub fn discover(
    directories: &[Utf8PathBuf],
    repository: &Utf8Path,
    graph: &WorkspaceGraph,
    symbols: Option<&ReferenceGraph>,
) -> Result<(Vec<Scenario>, Vec<RepoPath>), ScenarioError> {
    if directories.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let empty = ReferenceGraph::default();
    let symbols = symbols.unwrap_or(&empty);
    let routes = RouteTable::from_sources(repository, symbols);
    let repo_path = |path: &Utf8Path| -> Result<RepoPath, ScenarioError> {
        let relative = path
            .strip_prefix(repository)
            .map_err(|_| ScenarioError::OutsideRepository(path.to_owned()))?;
        Ok(RepoPath::parse(relative.as_str())?)
    };

    let mut scenarios = Vec::new();
    let mut roots = Vec::new();
    for directory in directories {
        let directory = directory.canonicalize_utf8()?;
        roots.push(repo_path(&directory)?);
        let mut files = Vec::new();
        collect_files(&directory, &mut files)?;
        files.sort();
        for file in files {
            let path = repo_path(&file)?;
            let requests = ScenarioRequests::read(&std::fs::read_to_string(&file)?);
            let package = graph.owner_of(&path).cloned();
            scenarios.push(Scenario::resolve(
                path, package, &requests, &routes, symbols,
            ));
        }
    }
    Ok((scenarios, roots))
}

fn collect_files(directory: &Utf8Path, files: &mut Vec<Utf8PathBuf>) -> std::io::Result<()> {
    for entry in directory.read_dir_utf8()? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_files(path, files)?;
        } else if SCENARIO_SUFFIXES
            .iter()
            .any(|suffix| path.as_str().ends_with(suffix))
        {
            files.push(path.to_owned());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_method_and_url_of_each_request() {
        let read = ScenarioRequests::read(
            r#"
name: "Feature CRUD"
config:
  headers:
    Content-Type: application/json
steps:
  - id: create
    request:
      method: post
      url: "/v1/features"
      body:
        url: "https://not-the-request.example"
    expect:
      status: 201
  - id: show
    request:
      url: /v1/features/{{vars.feature_id}}
      method: GET
"#,
        );

        assert!(!read.unreadable);
        let requests: Vec<(&str, &str)> = read
            .requests
            .iter()
            .map(|request| (request.method.as_str(), request.url.as_str()))
            .collect();
        assert_eq!(
            requests,
            [
                ("POST", "/v1/features"),
                ("GET", "/v1/features/{{vars.feature_id}}"),
            ]
        );
    }

    #[test]
    fn a_request_without_a_method_is_unreadable() {
        let read = ScenarioRequests::read("steps:\n  - request:\n      url: /v1/x\n");

        assert!(read.unreadable);
        assert!(read.requests.is_empty());
    }

    #[test]
    fn reads_the_top_level_fields_of_a_graphql_block() {
        let read = ScenarioRequests::read(
            r#"
steps:
  - id: create_feature
    request:
      method: POST
      url: /v1/graphql
      body:
        query: |
          mutation CreateFeature($input: CreateFeatureInput!) {
            created: createFeature(input: $input) { id name }
            # a comment { with braces }
            featureFlags(first: 10) { nodes { id } }
          }
        variables:
          input:
            name: "x"
"#,
        );

        assert_eq!(
            read.requests[0].graphql_fields,
            ["createFeature", "featureFlags"]
        );
    }

    #[test]
    fn graphql_field_names_become_snake_case() {
        assert_eq!(snake_case("createFeature"), "create_feature");
        assert_eq!(snake_case("me"), "me");
    }
}
