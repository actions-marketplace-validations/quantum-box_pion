//! Reads a SCIP index, the file `rust-analyzer scip` writes.
//!
//! SCIP is a protobuf message. Only the handful of fields pion needs are
//! decoded here, by hand, so that no protobuf dependency is pulled in:
//!
//! ```text
//! Index      { documents = 2 }
//! Document   { relative_path = 1, occurrences = 2 }
//! Occurrence { range = 1, symbol = 2, symbol_roles = 3, enclosing_range = 7 }
//! ```

use std::cmp::Ordering;

/// A zero-based position in a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

/// A half-open span of a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

impl Range {
    pub fn contains(&self, position: Position) -> bool {
        self.start <= position && position < self.end
    }

    pub fn contains_line(&self, line: u32) -> bool {
        self.start.line <= line && line <= self.end.line
    }

    /// Orders ranges so that an enclosing range sorts before what it encloses.
    pub fn outer_first(&self, other: &Self) -> Ordering {
        self.start
            .cmp(&other.start)
            .then_with(|| other.end.cmp(&self.end))
    }

    pub fn encloses(&self, other: &Self) -> bool {
        self.start <= other.start && other.end <= self.end
    }
}

/// One mention of a symbol in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    /// Where the symbol's name is written.
    pub range: Range,
    pub symbol: String,
    /// The occurrence defines the symbol, rather than referring to it.
    pub is_definition: bool,
    /// For a definition, the whole item: attributes, signature and body.
    pub enclosing: Option<Range>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    /// Path relative to the directory the index was built for.
    pub relative_path: String,
    pub occurrences: Vec<Occurrence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Index {
    pub documents: Vec<Document>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScipError {
    #[error("the index ends in the middle of a field")]
    Truncated,
    #[error("the index uses protobuf wire type {0}, which SCIP does not")]
    UnknownWireType(u8),
    #[error("a number in the index does not fit in 64 bits")]
    VarintTooLong,
    #[error("a range has {0} numbers; SCIP ranges have 3 or 4")]
    BadRange(usize),
    #[error("a path or symbol is not valid UTF-8")]
    NotUtf8,
}

const SYMBOL_ROLE_DEFINITION: u64 = 1;

impl Index {
    pub fn decode(bytes: &[u8]) -> Result<Self, ScipError> {
        let mut documents = Vec::new();
        for field in Fields::new(bytes) {
            if let (2, Value::Bytes(document)) = field? {
                documents.push(decode_document(document)?);
            }
        }
        Ok(Self { documents })
    }
}

fn decode_document(bytes: &[u8]) -> Result<Document, ScipError> {
    let mut relative_path = String::new();
    let mut occurrences = Vec::new();
    for field in Fields::new(bytes) {
        match field? {
            (1, Value::Bytes(path)) => relative_path = text(path)?,
            (2, Value::Bytes(occurrence)) => occurrences.push(decode_occurrence(occurrence)?),
            _ => {}
        }
    }
    Ok(Document {
        relative_path,
        occurrences,
    })
}

fn decode_occurrence(bytes: &[u8]) -> Result<Occurrence, ScipError> {
    let mut range = None;
    let mut symbol = String::new();
    let mut roles = 0;
    let mut enclosing = None;
    for field in Fields::new(bytes) {
        match field? {
            (1, Value::Bytes(packed)) => range = Some(decode_range(packed)?),
            (2, Value::Bytes(name)) => symbol = text(name)?,
            (3, Value::Varint(value)) => roles = value,
            (7, Value::Bytes(packed)) => enclosing = Some(decode_range(packed)?),
            _ => {}
        }
    }
    Ok(Occurrence {
        range: range.ok_or(ScipError::BadRange(0))?,
        symbol,
        is_definition: roles & SYMBOL_ROLE_DEFINITION != 0,
        enclosing,
    })
}

/// `[line, start, end]` on one line, or `[start_line, start, end_line, end]`.
fn decode_range(packed: &[u8]) -> Result<Range, ScipError> {
    let mut numbers = Vec::with_capacity(4);
    let mut rest = packed;
    while !rest.is_empty() {
        let (value, after) = varint(rest)?;
        numbers.push(u32::try_from(value).map_err(|_| ScipError::VarintTooLong)?);
        rest = after;
    }
    let position = |line, column| Position { line, column };
    match numbers[..] {
        [line, start, end] => Ok(Range {
            start: position(line, start),
            end: position(line, end),
        }),
        [start_line, start, end_line, end] => Ok(Range {
            start: position(start_line, start),
            end: position(end_line, end),
        }),
        _ => Err(ScipError::BadRange(numbers.len())),
    }
}

fn text(bytes: &[u8]) -> Result<String, ScipError> {
    String::from_utf8(bytes.to_vec()).map_err(|_| ScipError::NotUtf8)
}

enum Value<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    /// Fixed-width numbers, which pion never reads.
    Fixed,
}

/// The top-level fields of one protobuf message.
struct Fields<'a> {
    rest: &'a [u8],
}

impl<'a> Fields<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { rest: bytes }
    }

    fn next_field(&mut self) -> Result<(u64, Value<'a>), ScipError> {
        let (key, rest) = varint(self.rest)?;
        let number = key >> 3;
        // The mask leaves three bits, so the value always fits.
        let wire_type = (key & 0b111) as u8;
        let (value, rest) = match wire_type {
            0 => {
                let (value, rest) = varint(rest)?;
                (Value::Varint(value), rest)
            }
            1 => (Value::Fixed, skip(rest, 8)?),
            2 => {
                let (length, rest) = varint(rest)?;
                let length = usize::try_from(length).map_err(|_| ScipError::Truncated)?;
                if rest.len() < length {
                    return Err(ScipError::Truncated);
                }
                let (bytes, rest) = rest.split_at(length);
                (Value::Bytes(bytes), rest)
            }
            5 => (Value::Fixed, skip(rest, 4)?),
            other => return Err(ScipError::UnknownWireType(other)),
        };
        self.rest = rest;
        Ok((number, value))
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = Result<(u64, Value<'a>), ScipError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        let field = self.next_field();
        if field.is_err() {
            // Stop after the first error instead of reading garbage.
            self.rest = &[];
        }
        Some(field)
    }
}

fn skip(bytes: &[u8], count: usize) -> Result<&[u8], ScipError> {
    bytes.get(count..).ok_or(ScipError::Truncated)
}

fn varint(bytes: &[u8]) -> Result<(u64, &[u8]), ScipError> {
    let mut value = 0u64;
    for (index, byte) in bytes.iter().enumerate() {
        if index >= 10 {
            return Err(ScipError::VarintTooLong);
        }
        value |= u64::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            return Ok((value, &bytes[index + 1..]));
        }
    }
    Err(ScipError::Truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encodes one length-delimited field.
    fn field(number: u8, payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![number << 3 | 2, payload.len() as u8];
        bytes.extend_from_slice(payload);
        bytes
    }

    #[test]
    fn decodes_a_definition_with_its_enclosing_range() {
        let mut occurrence = field(1, &[7, 11, 21]);
        occurrence.extend(field(2, b"pkg is_expired()."));
        occurrence.extend([3 << 3, 1]);
        occurrence.extend(field(7, &[7, 4, 9, 5]));
        let mut document = field(1, b"src/token.rs");
        document.extend(field(2, &occurrence));
        let index = Index::decode(&field(2, &document)).expect("valid index");

        let position = |line, column| Position { line, column };
        assert_eq!(
            index.documents,
            [Document {
                relative_path: "src/token.rs".into(),
                occurrences: vec![Occurrence {
                    range: Range {
                        start: position(7, 11),
                        end: position(7, 21)
                    },
                    symbol: "pkg is_expired().".into(),
                    is_definition: true,
                    enclosing: Some(Range {
                        start: position(7, 4),
                        end: position(9, 5)
                    }),
                }],
            }]
        );
    }

    #[test]
    fn rejects_a_truncated_index() {
        // A document field that claims 5 bytes and has 1.
        assert_eq!(
            Index::decode(&[2 << 3 | 2, 5, 0]),
            Err(ScipError::Truncated)
        );
    }
}
