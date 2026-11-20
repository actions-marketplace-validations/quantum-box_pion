//! Which handler serves which HTTP route.
//!
//! Routes are read from source text in the shape axum uses:
//!
//! ```text
//! .route("/v1/items/:id", get(show_item).put(update_item))
//! ```
//!
//! The path comes from the string literal, the handler from the reference
//! graph: the first function mentioned after each method name. A route whose
//! path is not a literal (a constant, a `format!`) is left out, and a request
//! that matches no known route is reported as untraced by the caller.

use std::collections::BTreeSet;

use crate::change::RepoPath;
use crate::scip::Position;
use crate::symbols::{ReferenceGraph, SymbolId};

/// The method-router functions of axum's `routing` module.
const METHODS: [&str; 8] = [
    "get", "post", "put", "patch", "delete", "head", "options", "any",
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Literal(String),
    /// A path parameter in a route, or a template in a request.
    Any,
}

/// A path split into segments, for routes and for requests alike.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathPattern(Vec<Segment>);

impl PathPattern {
    /// A route path as axum writes it: `:id`, `{id}` and `*rest` are
    /// parameters.
    pub fn route(path: &str) -> Self {
        Self::split(path, |segment| {
            segment.starts_with(':') || segment.starts_with('{') || segment.starts_with('*')
        })
    }

    /// A request URL from a scenario. The scheme, host and query are dropped,
    /// and a segment with a `{{template}}` matches anything.
    pub fn request(url: &str) -> Self {
        let without_query = url.split(['?', '#']).next().unwrap_or("");
        let path = match without_query.split_once("://") {
            Some((_, rest)) => rest.split_once('/').map_or("", |(_, path)| path),
            None => without_query,
        };
        Self::split(path, |segment| segment.contains("{{"))
    }

    fn split(path: &str, is_any: impl Fn(&str) -> bool) -> Self {
        let segments = path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(|segment| {
                if is_any(segment) {
                    Segment::Any
                } else {
                    Segment::Literal(segment.to_owned())
                }
            })
            .collect();
        Self(segments)
    }

    /// Whether a request for `self` can reach the route `route`.
    ///
    /// A router can be nested under a prefix that the route's own literal
    /// does not show, so the route only has to match the end of the request.
    fn reaches(&self, route: &Self) -> bool {
        if route.0.is_empty() || route.0.len() > self.0.len() {
            return false;
        }
        let tail = &self.0[self.0.len() - route.0.len()..];
        tail.iter().zip(&route.0).all(|pair| match pair {
            (Segment::Literal(request), Segment::Literal(route)) => request == route,
            _ => true,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Route {
    /// Upper-case HTTP method, or `ANY`.
    method: String,
    path: PathPattern,
    handler: SymbolId,
}

/// The routes found in a workspace's sources.
#[derive(Debug, Default)]
pub struct RouteTable {
    routes: Vec<Route>,
}

impl RouteTable {
    /// Reads the routes of every indexed file. `read` returns a file's text
    /// as it was when the index was built, or `None` when it cannot be read;
    /// such a file is skipped.
    pub fn from_sources(
        symbols: &ReferenceGraph,
        read: impl Fn(&RepoPath) -> Option<String>,
    ) -> Self {
        let mut table = Self::default();
        for path in symbols.paths() {
            let Some(source) = read(path) else {
                continue;
            };
            if source.contains(".route(") {
                table.scan(path, &source, symbols);
            }
        }
        table
    }

    /// Adds the routes registered in one source file.
    pub fn scan(&mut self, path: &RepoPath, source: &str, symbols: &ReferenceGraph) {
        let lines = LineIndex::new(source);
        for call in route_calls(source) {
            let pattern = PathPattern::route(&call.path);
            for (index, method) in call.methods.iter().enumerate() {
                let end = call
                    .methods
                    .get(index + 1)
                    .map_or(call.end, |next| next.name_start);
                let handler = symbols
                    .references_between(
                        path,
                        lines.position(method.arguments_start),
                        lines.position(end),
                    )
                    .find(|reference| reference.symbol.as_str().ends_with("()."));
                if let Some(handler) = handler {
                    self.routes.push(Route {
                        method: method.name.to_uppercase(),
                        path: pattern.clone(),
                        handler: handler.symbol.clone(),
                    });
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    /// The handlers a request can reach.
    pub fn handlers(&self, method: &str, request: &PathPattern) -> BTreeSet<&SymbolId> {
        self.routes
            .iter()
            .filter(|route| route.method == "ANY" || route.method.eq_ignore_ascii_case(method))
            .filter(|route| request.reaches(&route.path))
            .map(|route| &route.handler)
            .collect()
    }
}

/// Byte offset -> line and column, both zero-based, columns in bytes.
struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(source: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(source.match_indices('\n').map(|(index, _)| index + 1));
        Self { starts }
    }

    fn position(&self, offset: usize) -> Position {
        let line = self.starts.partition_point(|start| *start <= offset) - 1;
        let column = offset - self.starts[line];
        Position {
            line: u32::try_from(line).unwrap_or(u32::MAX),
            column: u32::try_from(column).unwrap_or(u32::MAX),
        }
    }
}

struct MethodCall {
    name: String,
    /// Offset of the method's name.
    name_start: usize,
    /// Offset just past the method's `(`.
    arguments_start: usize,
}

struct RouteCall {
    path: String,
    methods: Vec<MethodCall>,
    /// Offset of the `)` that closes `.route(`.
    end: usize,
}

/// Finds every `.route("literal", …)` call in `source`.
fn route_calls(source: &str) -> Vec<RouteCall> {
    let bytes = source.as_bytes();
    let mut calls = Vec::new();
    for (start, _) in source.match_indices(".route(") {
        let mut cursor = start + ".route(".len();
        cursor = skip_whitespace(bytes, cursor);
        let Some((path, after_path)) = string_literal(source, cursor) else {
            continue;
        };
        let (methods, end) = scan_arguments(source, after_path);
        calls.push(RouteCall { path, methods, end });
    }
    calls
}

fn skip_whitespace(bytes: &[u8], mut cursor: usize) -> usize {
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    cursor
}

/// Reads a plain `"…"` literal at `start`. Returns its content and the
/// offset after the closing quote.
fn string_literal(source: &str, start: usize) -> Option<(String, usize)> {
    let rest = source.get(start..)?.strip_prefix('"')?;
    let mut content = String::new();
    let mut chars = rest.char_indices();
    while let Some((index, c)) = chars.next() {
        match c {
            '"' => return Some((content, start + 1 + index + 1)),
            '\\' => content.push(chars.next()?.1),
            _ => content.push(c),
        }
    }
    None
}

/// Walks the rest of a `.route(` call from `start` to its closing paren,
/// collecting the method-router calls on the way.
fn scan_arguments(source: &str, start: usize) -> (Vec<MethodCall>, usize) {
    let bytes = source.as_bytes();
    let mut methods = Vec::new();
    let mut depth = 1usize;
    let mut cursor = start;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'"' => {
                cursor = string_literal(source, cursor).map_or(bytes.len(), |(_, after)| after);
                continue;
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return (methods, cursor);
                }
            }
            byte if is_identifier_byte(byte) => {
                let name_start = cursor;
                while bytes.get(cursor).is_some_and(|b| is_identifier_byte(*b)) {
                    cursor += 1;
                }
                let name = &source[name_start..cursor];
                let after = skip_whitespace(bytes, cursor);
                if METHODS.contains(&name) && bytes.get(after) == Some(&b'(') {
                    methods.push(MethodCall {
                        name: name.to_owned(),
                        name_start,
                        arguments_start: after + 1,
                    });
                }
                continue;
            }
            _ => {}
        }
        cursor += 1;
    }
    (methods, bytes.len())
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_reaches_a_route_through_parameters_and_prefixes() {
        let request = PathPattern::request("https://api.example/v1/items/42?verbose=1");

        assert!(request.reaches(&PathPattern::route("/v1/items/:id")));
        assert!(request.reaches(&PathPattern::route("/v1/items/{id}")));
        // The router that owns `/items/:id` may be nested under `/v1`.
        assert!(request.reaches(&PathPattern::route("/items/:id")));
        assert!(!request.reaches(&PathPattern::route("/v1/orders/:id")));
        assert!(!request.reaches(&PathPattern::route("/v0/v1/items/:id")));
        assert!(!request.reaches(&PathPattern::route("/")));
    }

    #[test]
    fn a_templated_request_segment_matches_any_route_segment() {
        let request = PathPattern::request("/v1/items/{{vars.item_id}}/tags");

        assert!(request.reaches(&PathPattern::route("/v1/items/:id/tags")));
        assert!(request.reaches(&PathPattern::route("/v1/items/archived/tags")));
    }

    #[test]
    fn route_calls_are_read_with_chained_methods() {
        let source = r#"
            Router::new()
                .route(
                    "/v1/items/:id",
                    get(show_item).put(update_item),
                )
                .route(DYNAMIC_PATH, post(skipped))
                .route("/v1/health", get(|| async { "ok (fine)" }))
        "#;
        let calls = route_calls(source);

        let summary: Vec<(&str, Vec<&str>)> = calls
            .iter()
            .map(|call| {
                let methods = call.methods.iter().map(|m| m.name.as_str()).collect();
                (call.path.as_str(), methods)
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("/v1/items/:id", vec!["get", "put"]),
                ("/v1/health", vec!["get"]),
            ]
        );
    }
}
