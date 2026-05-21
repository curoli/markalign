//! `markalign` compares Markdown documents at the syntax level instead of as
//! raw source text.
//!
//! ```
//! use markalign::{compare_pair, normalize_document, Document, Options};
//!
//! # fn main() -> Result<(), markalign::Error> {
//! let options = Options::default();
//! let reference = Document::with_id("reference", "Hello *world*.");
//! let alternative = Document::with_id("alternative", "Hello _world_.");
//!
//! let normalized = normalize_document(&reference, &options)?;
//! let comparison = compare_pair(&reference, &alternative, &options)?;
//!
//! assert!(!normalized.tokens.is_empty());
//! assert!(comparison.comparisons[0].substitutions.is_empty());
//! # Ok(())
//! # }
//! ```

use core::ops::Range;
use pulldown_cmark::{
    CodeBlockKind, Event, HeadingLevel, Options as MarkdownOptions, Parser, Tag, TagEnd,
};
use serde::{Deserialize, Serialize};
use similar::{Algorithm, DiffTag, capture_diff_slices};
use unicode_normalization::UnicodeNormalization;

/// One source document provided to `markalign`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    /// Optional identifier supplied by the caller.
    pub id: Option<String>,
    /// Original source text as provided by the caller.
    pub source: String,
}

impl Document {
    /// Creates a new document from source text.
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            id: None,
            source: source.into(),
        }
    }

    /// Creates a new document with a caller-provided identifier.
    pub fn with_id(id: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            id: Some(id.into()),
            source: source.into(),
        }
    }
}

/// Configuration for parsing, normalization, and consolidation.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Options {
    /// Whether Unicode normalization should be applied before parsing.
    pub normalize_unicode: bool,
    /// Whether ordered and unordered lists should be treated as equivalent.
    pub equate_list_kinds: bool,
    /// Maximum size of an equality region that may be absorbed into a nearby
    /// substitution during consolidation.
    pub absorb_equalities_up_to: usize,
}

/// A normalized syntax token derived from parsed Markdown.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum Token {
    Text(String),
    Start(StructureKind),
    End(StructureKind),
    Atom(AtomKind),
}

/// Structural or formatting region markers used in the normalized token stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum StructureKind {
    Paragraph,
    Heading { level: u8 },
    BlockQuote,
    Emphasis,
    Strong,
    Link,
    Image,
    List { ordered: bool },
    ListItem,
    CodeSpan,
    CodeBlock,
}

/// Atomic tokens that do not enclose child content.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum AtomKind {
    SoftBreak,
    HardBreak,
    Rule,
    InlineCode(String),
    CodeBlockLanguage(Option<String>),
    LinkDestination { destination: String, title: String },
    ImageDestination { destination: String, title: String },
}

/// A normalized document ready for token-level comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedDocument {
    pub document_id: Option<String>,
    pub source: String,
    pub tokens: Vec<Token>,
    /// Source byte ranges parallel to `tokens`.
    pub token_ranges: Vec<Range<usize>>,
}

impl NormalizedDocument {
    /// Returns a `SourceMap` view for converting byte ranges into line/column spans.
    pub fn source_map(&self) -> SourceMap<'_> {
        SourceMap::new(&self.source)
    }

    /// Returns source spans parallel to `tokens`.
    pub fn token_spans(&self) -> Vec<SourceSpan> {
        let map = self.source_map();
        self.token_ranges
            .iter()
            .cloned()
            .map(|range| map.span_for_range(range))
            .collect()
    }
}

/// One replacement that transforms a region of the reference into a region of
/// an alternative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Substitution {
    /// Token range in the reference document to be replaced.
    pub reference_range: Range<usize>,
    /// Source byte range in the reference document covered by `reference_range`.
    pub reference_source_range: Range<usize>,
    /// Source byte range in the alternative document covered by `replacement`.
    pub alternative_source_range: Range<usize>,
    /// Replacement tokens taken from the alternative document.
    pub replacement: Vec<Token>,
}

impl Substitution {
    /// Converts the reference source byte range into a structured span.
    pub fn reference_span(&self, reference: &NormalizedDocument) -> SourceSpan {
        reference
            .source_map()
            .span_for_range(self.reference_source_range.clone())
    }

    /// Converts the alternative source byte range into a structured span.
    pub fn alternative_span(&self, alternative: &NormalizedDocument) -> SourceSpan {
        alternative
            .source_map()
            .span_for_range(self.alternative_source_range.clone())
    }
}

/// Final result for comparing one alternative document against the reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comparison {
    pub alternative_id: Option<String>,
    pub substitutions: Vec<Substitution>,
}

/// One source location in a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourcePosition {
    pub byte: usize,
    pub line: usize,
    pub column: usize,
}

/// A source span with both byte offsets and line/column positions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpan {
    pub range: Range<usize>,
    pub start: SourcePosition,
    pub end: SourcePosition,
}

/// Lightweight mapping from byte offsets to line/column positions.
#[derive(Debug, Clone, Copy)]
pub struct SourceMap<'a> {
    source: &'a str,
}

impl<'a> SourceMap<'a> {
    pub fn new(source: &'a str) -> Self {
        Self { source }
    }

    pub fn span_for_range(&self, range: Range<usize>) -> SourceSpan {
        SourceSpan {
            start: self.position_at(range.start),
            end: self.position_at(range.end),
            range,
        }
    }

    pub fn position_at(&self, byte: usize) -> SourcePosition {
        let clamped = byte.min(self.source.len());
        let mut line = 1;
        let mut column = 1;

        for ch in self.source[..clamped].chars() {
            if ch == '\n' {
                line += 1;
                column = 1;
            } else {
                column += 1;
            }
        }

        SourcePosition {
            byte: clamped,
            line,
            column,
        }
    }
}

/// Full output for one comparison run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonSet {
    pub reference: NormalizedDocument,
    pub comparisons: Vec<Comparison>,
}

/// Errors that future parsing, normalization, and comparison steps may report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum Error {
    EmptyInput,
    ParseFailed {
        document_id: Option<String>,
        message: String,
    },
    UnsupportedFeature {
        message: String,
    },
}

pub fn normalize_document(
    document: &Document,
    options: &Options,
) -> Result<NormalizedDocument, Error> {
    if document.source.is_empty() {
        return Err(Error::EmptyInput);
    }

    let source = if options.normalize_unicode {
        document.source.nfc().collect::<String>()
    } else {
        document.source.clone()
    };

    let mut tokens = Vec::new();
    let mut token_ranges = Vec::new();

    for (event, source_range) in
        Parser::new_ext(&source, MarkdownOptions::empty()).into_offset_iter()
    {
        push_event(&mut tokens, &mut token_ranges, event, source_range, options)?;
    }

    Ok(NormalizedDocument {
        document_id: document.id.clone(),
        source,
        tokens,
        token_ranges,
    })
}

/// Compares one reference document against one alternative document.
pub fn compare_pair(
    reference: &Document,
    alternative: &Document,
    options: &Options,
) -> Result<ComparisonSet, Error> {
    let reference = normalize_document(reference, options)?;
    let alternative = normalize_document(alternative, options)?;
    let substitutions = diff_documents(&reference, &alternative, options);

    Ok(ComparisonSet {
        reference,
        comparisons: vec![Comparison {
            alternative_id: alternative.document_id,
            substitutions,
        }],
    })
}

/// Compares one reference document against zero or more alternatives.
pub fn compare_many(
    reference: &Document,
    alternatives: &[Document],
    options: &Options,
) -> Result<ComparisonSet, Error> {
    let reference = normalize_document(reference, options)?;
    let mut comparisons = Vec::with_capacity(alternatives.len());

    for alternative in alternatives {
        let normalized = normalize_document(alternative, options)?;
        let substitutions = diff_documents(&reference, &normalized, options);

        comparisons.push(Comparison {
            alternative_id: normalized.document_id,
            substitutions,
        });
    }

    Ok(ComparisonSet {
        reference,
        comparisons,
    })
}

fn diff_documents(
    reference: &NormalizedDocument,
    alternative: &NormalizedDocument,
    options: &Options,
) -> Vec<Substitution> {
    let ops = capture_diff_slices(Algorithm::Myers, &reference.tokens, &alternative.tokens);
    let mut substitutions = Vec::new();
    let mut pending: Option<PendingSubstitution> = None;

    for (index, op) in ops.iter().enumerate() {
        match op.tag() {
            DiffTag::Equal => {
                let should_absorb = pending.is_some()
                    && op.old_range().len() <= options.absorb_equalities_up_to
                    && has_following_change(&ops[index + 1..]);

                if should_absorb {
                    let old_range = op.old_range();
                    let new_range = op.new_range();
                    pending
                        .as_mut()
                        .expect("pending substitution should exist")
                        .extend(old_range, new_range);
                } else if let Some(pending) = pending.take() {
                    substitutions.push(pending.finish(reference, alternative));
                }
            }
            DiffTag::Delete | DiffTag::Insert | DiffTag::Replace => {
                let old_range = op.old_range();
                let new_range = op.new_range();
                pending
                    .get_or_insert_with(|| {
                        PendingSubstitution::new(old_range.start, new_range.start)
                    })
                    .extend(old_range, new_range);
            }
        }
    }

    if let Some(pending) = pending.take() {
        substitutions.push(pending.finish(reference, alternative));
    }

    substitutions
}

fn has_following_change(ops: &[similar::DiffOp]) -> bool {
    ops.iter().any(|op| op.tag() != DiffTag::Equal)
}

#[derive(Debug, Clone)]
struct PendingSubstitution {
    reference_start: usize,
    reference_end: usize,
    alternative_start: usize,
    alternative_end: usize,
}

impl PendingSubstitution {
    fn new(reference_start: usize, alternative_start: usize) -> Self {
        Self {
            reference_start,
            reference_end: reference_start,
            alternative_start,
            alternative_end: alternative_start,
        }
    }

    fn extend(&mut self, reference_range: Range<usize>, alternative_range: Range<usize>) {
        self.reference_end = reference_range.end;
        self.alternative_end = alternative_range.end;
    }

    fn finish(
        self,
        reference: &NormalizedDocument,
        alternative: &NormalizedDocument,
    ) -> Substitution {
        Substitution {
            reference_range: self.reference_start..self.reference_end,
            reference_source_range: source_span_for_token_range(
                &reference.token_ranges,
                self.reference_start..self.reference_end,
            ),
            alternative_source_range: source_span_for_token_range(
                &alternative.token_ranges,
                self.alternative_start..self.alternative_end,
            ),
            replacement: alternative.tokens[self.alternative_start..self.alternative_end].to_vec(),
        }
    }
}

fn source_span_for_token_range(
    token_ranges: &[Range<usize>],
    token_range: Range<usize>,
) -> Range<usize> {
    if token_ranges.is_empty() {
        return 0..0;
    }

    if token_range.is_empty() {
        if token_range.start == 0 {
            let boundary = token_ranges[0].start;
            return boundary..boundary;
        }

        if token_range.start >= token_ranges.len() {
            let boundary = token_ranges[token_ranges.len() - 1].end;
            return boundary..boundary;
        }

        let boundary = token_ranges[token_range.start - 1].end;
        return boundary..boundary;
    }

    token_ranges[token_range.start].start..token_ranges[token_range.end - 1].end
}

fn push_event(
    tokens: &mut Vec<Token>,
    token_ranges: &mut Vec<Range<usize>>,
    event: Event<'_>,
    source_range: Range<usize>,
    options: &Options,
) -> Result<(), Error> {
    match event {
        Event::Start(tag) => push_start_tag(tokens, token_ranges, tag, source_range, options),
        Event::End(tag_end) => push_end_tag(tokens, token_ranges, tag_end, source_range, options),
        Event::Text(text) => {
            push_text_tokens(tokens, token_ranges, text.as_ref(), source_range);
            Ok(())
        }
        Event::Code(code) => {
            push_token(
                tokens,
                token_ranges,
                Token::Atom(AtomKind::InlineCode(code.into_string())),
                source_range,
            );
            Ok(())
        }
        Event::SoftBreak => {
            push_token(
                tokens,
                token_ranges,
                Token::Atom(AtomKind::SoftBreak),
                source_range,
            );
            Ok(())
        }
        Event::HardBreak => {
            push_token(
                tokens,
                token_ranges,
                Token::Atom(AtomKind::HardBreak),
                source_range,
            );
            Ok(())
        }
        Event::Rule => {
            push_token(
                tokens,
                token_ranges,
                Token::Atom(AtomKind::Rule),
                source_range,
            );
            Ok(())
        }
        Event::InlineMath(_) | Event::DisplayMath(_) => Err(Error::UnsupportedFeature {
            message: "math events are not supported in v1".to_string(),
        }),
        Event::Html(html) | Event::InlineHtml(html) => Err(Error::UnsupportedFeature {
            message: format!("HTML is not supported in v1: {html:?}"),
        }),
        Event::FootnoteReference(label) => Err(Error::UnsupportedFeature {
            message: format!("footnotes are not supported in v1: {label}"),
        }),
        Event::TaskListMarker(_) => Err(Error::UnsupportedFeature {
            message: "task lists are not supported in v1".to_string(),
        }),
    }
}

fn push_start_tag(
    tokens: &mut Vec<Token>,
    token_ranges: &mut Vec<Range<usize>>,
    tag: Tag<'_>,
    source_range: Range<usize>,
    options: &Options,
) -> Result<(), Error> {
    match tag {
        Tag::Paragraph => push_token(
            tokens,
            token_ranges,
            Token::Start(StructureKind::Paragraph),
            source_range,
        ),
        Tag::Heading { level, .. } => push_token(
            tokens,
            token_ranges,
            Token::Start(StructureKind::Heading {
                level: heading_level_to_u8(level),
            }),
            source_range,
        ),
        Tag::BlockQuote(_) => push_token(
            tokens,
            token_ranges,
            Token::Start(StructureKind::BlockQuote),
            source_range,
        ),
        Tag::CodeBlock(kind) => match kind {
            CodeBlockKind::Indented => push_token(
                tokens,
                token_ranges,
                Token::Start(StructureKind::CodeBlock),
                source_range,
            ),
            CodeBlockKind::Fenced(info) => {
                let language = fenced_code_language(info.as_ref());
                push_token(
                    tokens,
                    token_ranges,
                    Token::Start(StructureKind::CodeBlock),
                    source_range.clone(),
                );
                push_token(
                    tokens,
                    token_ranges,
                    Token::Atom(AtomKind::CodeBlockLanguage(language)),
                    source_range,
                );
            }
        },
        Tag::List(first_item_number) => {
            let ordered = if options.equate_list_kinds {
                false
            } else {
                first_item_number.is_some()
            };
            push_token(
                tokens,
                token_ranges,
                Token::Start(StructureKind::List { ordered }),
                source_range,
            );
        }
        Tag::Item => push_token(
            tokens,
            token_ranges,
            Token::Start(StructureKind::ListItem),
            source_range,
        ),
        Tag::Emphasis => push_token(
            tokens,
            token_ranges,
            Token::Start(StructureKind::Emphasis),
            source_range,
        ),
        Tag::Strong => push_token(
            tokens,
            token_ranges,
            Token::Start(StructureKind::Strong),
            source_range,
        ),
        Tag::Link {
            dest_url, title, ..
        } => {
            push_token(
                tokens,
                token_ranges,
                Token::Start(StructureKind::Link),
                source_range.clone(),
            );
            push_token(
                tokens,
                token_ranges,
                Token::Atom(AtomKind::LinkDestination {
                    destination: dest_url.into_string(),
                    title: title.into_string(),
                }),
                source_range,
            );
        }
        Tag::Image {
            dest_url, title, ..
        } => {
            push_token(
                tokens,
                token_ranges,
                Token::Start(StructureKind::Image),
                source_range.clone(),
            );
            push_token(
                tokens,
                token_ranges,
                Token::Atom(AtomKind::ImageDestination {
                    destination: dest_url.into_string(),
                    title: title.into_string(),
                }),
                source_range,
            );
        }
        unsupported => {
            return Err(Error::UnsupportedFeature {
                message: format!("unsupported start tag in v1: {unsupported:?}"),
            });
        }
    }

    Ok(())
}

fn push_end_tag(
    tokens: &mut Vec<Token>,
    token_ranges: &mut Vec<Range<usize>>,
    tag_end: TagEnd,
    source_range: Range<usize>,
    options: &Options,
) -> Result<(), Error> {
    match tag_end {
        TagEnd::Paragraph => push_token(
            tokens,
            token_ranges,
            Token::End(StructureKind::Paragraph),
            source_range,
        ),
        TagEnd::Heading(level) => push_token(
            tokens,
            token_ranges,
            Token::End(StructureKind::Heading {
                level: heading_level_to_u8(level),
            }),
            source_range,
        ),
        TagEnd::BlockQuote(_) => push_token(
            tokens,
            token_ranges,
            Token::End(StructureKind::BlockQuote),
            source_range,
        ),
        TagEnd::CodeBlock => push_token(
            tokens,
            token_ranges,
            Token::End(StructureKind::CodeBlock),
            source_range,
        ),
        TagEnd::List(is_ordered) => push_token(
            tokens,
            token_ranges,
            Token::End(StructureKind::List {
                ordered: if options.equate_list_kinds {
                    false
                } else {
                    is_ordered
                },
            }),
            source_range,
        ),
        TagEnd::Item => push_token(
            tokens,
            token_ranges,
            Token::End(StructureKind::ListItem),
            source_range,
        ),
        TagEnd::Emphasis => push_token(
            tokens,
            token_ranges,
            Token::End(StructureKind::Emphasis),
            source_range,
        ),
        TagEnd::Strong => push_token(
            tokens,
            token_ranges,
            Token::End(StructureKind::Strong),
            source_range,
        ),
        TagEnd::Link => push_token(
            tokens,
            token_ranges,
            Token::End(StructureKind::Link),
            source_range,
        ),
        TagEnd::Image => push_token(
            tokens,
            token_ranges,
            Token::End(StructureKind::Image),
            source_range,
        ),
        unsupported => {
            return Err(Error::UnsupportedFeature {
                message: format!("unsupported end tag in v1: {unsupported:?}"),
            });
        }
    }

    Ok(())
}

fn push_token(
    tokens: &mut Vec<Token>,
    token_ranges: &mut Vec<Range<usize>>,
    token: Token,
    source_range: Range<usize>,
) {
    tokens.push(token);
    token_ranges.push(source_range);
}

fn push_text_tokens(
    tokens: &mut Vec<Token>,
    token_ranges: &mut Vec<Range<usize>>,
    text: &str,
    source_range: Range<usize>,
) {
    for chunk in text_chunks(text) {
        push_token(
            tokens,
            token_ranges,
            Token::Text(text[chunk.clone()].to_string()),
            source_range.start + chunk.start..source_range.start + chunk.end,
        );
    }
}

fn text_chunks(text: &str) -> Vec<Range<usize>> {
    let mut chunks = Vec::new();
    let mut current_start = None;
    let mut current_kind = None;

    for (index, ch) in text.char_indices() {
        let kind = TextChunkKind::for_char(ch);

        if current_kind.is_some_and(|current| current == kind) {
            continue;
        }

        if let Some(start) = current_start {
            chunks.push(start..index);
        }

        current_start = Some(index);
        current_kind = Some(kind);
    }

    if let Some(start) = current_start {
        chunks.push(start..text.len());
    }

    chunks
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextChunkKind {
    Word,
    Whitespace,
    Punctuation,
}

impl TextChunkKind {
    fn for_char(ch: char) -> Self {
        if ch.is_alphanumeric() {
            Self::Word
        } else if ch.is_whitespace() {
            Self::Whitespace
        } else {
            Self::Punctuation
        }
    }
}

fn heading_level_to_u8(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn fenced_code_language(info: &str) -> Option<String> {
    info.split_whitespace()
        .next()
        .filter(|segment| !segment.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_equivalent_emphasis_sources_identically() {
        let options = Options::default();
        let star = Document::new("Hello *world*.");
        let underscore = Document::new("Hello _world_.");

        let left = normalize_document(&star, &options).unwrap();
        let right = normalize_document(&underscore, &options).unwrap();

        assert_eq!(left.tokens, right.tokens);
    }

    #[test]
    fn preserves_link_destination_in_tokens() {
        let options = Options::default();
        let document = Document::new("[home](https://example.com \"Example\")");
        let normalized = normalize_document(&document, &options).unwrap();

        assert_eq!(
            normalized.tokens,
            vec![
                Token::Start(StructureKind::Paragraph),
                Token::Start(StructureKind::Link),
                Token::Atom(AtomKind::LinkDestination {
                    destination: "https://example.com".to_string(),
                    title: "Example".to_string(),
                }),
                Token::Text("home".to_string()),
                Token::End(StructureKind::Link),
                Token::End(StructureKind::Paragraph),
            ]
        );
    }

    #[test]
    fn can_equate_list_kinds_when_configured() {
        let options = Options {
            equate_list_kinds: true,
            ..Options::default()
        };
        let bullet = Document::new("- one\n- two\n");
        let ordered = Document::new("1. one\n2. two\n");

        let left = normalize_document(&bullet, &options).unwrap();
        let right = normalize_document(&ordered, &options).unwrap();

        assert_eq!(left.tokens, right.tokens);
    }

    #[test]
    fn compare_pair_returns_empty_substitutions_for_identical_normalization() {
        let options = Options::default();
        let reference = Document::new("Hello *world*.");
        let alternative = Document::new("Hello _world_.");

        let result = compare_pair(&reference, &alternative, &options).unwrap();

        assert_eq!(result.comparisons.len(), 1);
        assert!(result.comparisons[0].substitutions.is_empty());
    }

    #[test]
    fn compare_pair_returns_local_word_substitution() {
        let options = Options::default();
        let reference = Document::new("Hello *world*.");
        let alternative = Document::new("Hello *there*.");

        let result = compare_pair(&reference, &alternative, &options).unwrap();

        assert_eq!(result.comparisons[0].substitutions.len(), 1);
        assert_eq!(
            result.comparisons[0].substitutions[0],
            Substitution {
                reference_range: 4..5,
                reference_source_range: 7..12,
                alternative_source_range: 7..12,
                replacement: vec![Token::Text("there".to_string())],
            }
        );
    }

    #[test]
    fn compare_pair_can_absorb_small_equalities() {
        let options = Options {
            absorb_equalities_up_to: 1,
            ..Options::default()
        };
        let reference = NormalizedDocument {
            document_id: None,
            source: "abcd".to_string(),
            tokens: vec![
                Token::Text("a".to_string()),
                Token::Text("b".to_string()),
                Token::Text("c".to_string()),
                Token::Text("d".to_string()),
            ],
            token_ranges: vec![0..1, 1..2, 2..3, 3..4],
        };
        let alternative = NormalizedDocument {
            document_id: None,
            source: "axcy".to_string(),
            tokens: vec![
                Token::Text("a".to_string()),
                Token::Text("x".to_string()),
                Token::Text("c".to_string()),
                Token::Text("y".to_string()),
            ],
            token_ranges: vec![0..1, 1..2, 2..3, 3..4],
        };

        assert_eq!(
            diff_documents(&reference, &alternative, &options),
            vec![Substitution {
                reference_range: 1..4,
                reference_source_range: 1..4,
                alternative_source_range: 1..4,
                replacement: vec![
                    Token::Text("x".to_string()),
                    Token::Text("c".to_string()),
                    Token::Text("y".to_string()),
                ],
            }]
        );
    }

    #[test]
    fn normalized_document_tracks_source_ranges() {
        let options = Options::default();
        let document = Document::new("Hello *world*.");
        let normalized = normalize_document(&document, &options).unwrap();

        assert_eq!(normalized.tokens.len(), normalized.token_ranges.len());
        assert_eq!(normalized.token_ranges[0], 0..14);
        assert_eq!(normalized.token_ranges[4], 7..12);
    }

    #[test]
    fn text_is_split_into_word_space_and_punctuation_chunks() {
        let options = Options::default();
        let document = Document::new("Hello world.");
        let normalized = normalize_document(&document, &options).unwrap();

        assert_eq!(
            normalized.tokens,
            vec![
                Token::Start(StructureKind::Paragraph),
                Token::Text("Hello".to_string()),
                Token::Text(" ".to_string()),
                Token::Text("world".to_string()),
                Token::Text(".".to_string()),
                Token::End(StructureKind::Paragraph),
            ]
        );
        assert_eq!(normalized.token_ranges[3], 6..11);
    }

    #[test]
    fn source_map_reports_line_and_column_positions() {
        let map = SourceMap::new("alpha\nbeta\n");

        assert_eq!(
            map.position_at(0),
            SourcePosition {
                byte: 0,
                line: 1,
                column: 1,
            }
        );
        assert_eq!(
            map.position_at(6),
            SourcePosition {
                byte: 6,
                line: 2,
                column: 1,
            }
        );
        assert_eq!(
            map.position_at(10),
            SourcePosition {
                byte: 10,
                line: 2,
                column: 5,
            }
        );
    }

    #[test]
    fn substitution_can_be_mapped_to_structured_spans() {
        let options = Options::default();
        let reference = normalize_document(&Document::new("Hello *world*."), &options).unwrap();
        let alternative = normalize_document(&Document::new("Hello *there*."), &options).unwrap();
        let result = compare_pair(
            &Document::new("Hello *world*."),
            &Document::new("Hello *there*."),
            &options,
        )
        .unwrap();
        let substitution = &result.comparisons[0].substitutions[0];

        assert_eq!(
            substitution.reference_span(&reference),
            SourceSpan {
                range: 7..12,
                start: SourcePosition {
                    byte: 7,
                    line: 1,
                    column: 8,
                },
                end: SourcePosition {
                    byte: 12,
                    line: 1,
                    column: 13,
                },
            }
        );
        assert_eq!(
            substitution.alternative_span(&alternative),
            SourceSpan {
                range: 7..12,
                start: SourcePosition {
                    byte: 7,
                    line: 1,
                    column: 8,
                },
                end: SourcePosition {
                    byte: 12,
                    line: 1,
                    column: 13,
                },
            }
        );
    }

    #[test]
    fn comparison_set_round_trips_through_json() {
        let options = Options::default();
        let result = compare_pair(
            &Document::with_id("reference", "Hello *world*."),
            &Document::with_id("alternative", "Hello *there*."),
            &options,
        )
        .unwrap();

        let json = serde_json::to_string(&result).unwrap();
        let decoded: ComparisonSet = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, result);
    }

    #[test]
    fn serialized_comparison_uses_stable_field_names() {
        let options = Options::default();
        let result = compare_pair(
            &Document::with_id("reference", "Hello *world*."),
            &Document::with_id("alternative", "Hello *there*."),
            &options,
        )
        .unwrap();
        let json = serde_json::to_value(&result).unwrap();
        let substitution = &json["comparisons"][0]["substitutions"][0];

        assert_eq!(json["reference"]["document_id"], "reference");
        assert_eq!(json["comparisons"][0]["alternative_id"], "alternative");
        assert_eq!(substitution["reference_range"]["start"], 4);
        assert_eq!(substitution["reference_range"]["end"], 5);
        assert_eq!(substitution["reference_source_range"]["start"], 7);
        assert_eq!(substitution["reference_source_range"]["end"], 12);
        assert_eq!(substitution["replacement"][0]["kind"], "Text");
        assert_eq!(substitution["replacement"][0]["data"], "there");
    }
}
