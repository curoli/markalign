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
use std::collections::BTreeMap;

use pulldown_cmark::{
    Alignment, CodeBlockKind, Event, HeadingLevel, Options as MarkdownOptions, Parser, Tag, TagEnd,
};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use similar::{Algorithm, DiffTag, capture_diff_slices};
use unicode_normalization::UnicodeNormalization;

/// One source document provided to `markalign`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
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
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Options {
    /// Whether Unicode normalization should be applied before parsing.
    pub normalize_unicode: bool,
    /// Whether ordered and unordered lists should be treated as equivalent.
    pub equate_list_kinds: bool,
    /// Maximum size of an equality region that may be absorbed into a nearby
    /// substitution during consolidation.
    pub absorb_equalities_up_to: usize,
    /// When enabled, parser extensions such as tables, footnotes, task lists,
    /// and math are turned on.
    pub enable_extended_markdown: bool,
    /// When enabled, comparison output emphasizes block-level change regions.
    pub block_level_changes_only: bool,
    /// Small gaps between change regions can be merged for UI-level grouping.
    pub merge_adjacent_regions_up_to: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            normalize_unicode: false,
            equate_list_kinds: false,
            absorb_equalities_up_to: 0,
            enable_extended_markdown: true,
            block_level_changes_only: false,
            merge_adjacent_regions_up_to: 1,
        }
    }
}

/// A normalized syntax token derived from parsed Markdown.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "kind", content = "data"))]
pub enum Token {
    Text(String),
    Start(StructureKind),
    End(StructureKind),
    Atom(AtomKind),
}

/// Structural or formatting region markers used in the normalized token stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "kind", content = "data"))]
pub enum StructureKind {
    Paragraph,
    Heading { level: u8 },
    BlockQuote,
    Emphasis,
    Strong,
    Strikethrough,
    Link,
    Image,
    List { ordered: bool },
    ListItem,
    CodeBlock,
    Table,
    TableHead,
    TableRow,
    TableCell,
    HtmlBlock,
    FootnoteDefinition,
}

/// Atomic tokens that do not enclose child content.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "kind", content = "data"))]
pub enum AtomKind {
    SoftBreak,
    HardBreak,
    Rule,
    InlineCode(String),
    CodeBlockLanguage(Option<String>),
    TableAlignment(Vec<TableAlignmentKind>),
    LinkDestination { destination: String, title: String },
    ImageDestination { destination: String, title: String },
    InlineHtml(String),
    Html(String),
    FootnoteReference(String),
    TaskListMarker(bool),
    InlineMath(String),
    DisplayMath(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum TableAlignmentKind {
    None,
    Left,
    Center,
    Right,
}

/// A normalized document ready for token-level comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
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

impl ComparisonSet {
    pub fn comparison_by_id(&self, alternative_id: &str) -> Option<&Comparison> {
        self.comparison_index
            .get(alternative_id)
            .and_then(|index| self.comparisons.get(*index))
    }
}

/// One replacement that transforms a region of the reference into a region of
/// an alternative.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
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
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Comparison {
    pub alternative_id: Option<String>,
    pub alternative_blocks: Vec<ReferenceBlock>,
    pub substitutions: Vec<Substitution>,
    pub changed_regions: Vec<ChangeRegion>,
    pub unchanged_regions: Vec<UnchangedRegion>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum BlockKind {
    Paragraph,
    Heading,
    BlockQuote,
    ListItem,
    CodeBlock,
    TableRow,
    HtmlBlock,
    FootnoteDefinition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct BlockAnchor {
    pub block_index: usize,
    pub block_path: Vec<usize>,
    pub heading_path: Vec<usize>,
    pub list_item_index: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ReferenceBlock {
    pub index: usize,
    pub kind: BlockKind,
    pub token_range: Range<usize>,
    pub source_range: Range<usize>,
    pub anchor: BlockAnchor,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum ChangeWeight {
    Small,
    Medium,
    Large,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ChangeRegion {
    pub reference_range: Range<usize>,
    pub reference_source_range: Range<usize>,
    pub alternative_source_range: Range<usize>,
    pub block_indices: Vec<usize>,
    pub block_kinds: Vec<BlockKind>,
    pub primary_anchor: Option<BlockAnchor>,
    pub weight: ChangeWeight,
    pub replacement: Vec<Token>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct UnchangedRegion {
    pub reference_range: Range<usize>,
    pub reference_source_range: Range<usize>,
    pub block_indices: Vec<usize>,
    pub block_kinds: Vec<BlockKind>,
    pub primary_anchor: Option<BlockAnchor>,
}

/// One source location in a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct SourcePosition {
    pub byte: usize,
    pub line: usize,
    pub column: usize,
}

/// A source span with both byte offsets and line/column positions.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
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
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ComparisonSet {
    pub reference: NormalizedDocument,
    pub reference_blocks: Vec<ReferenceBlock>,
    pub comparisons: Vec<Comparison>,
    pub comparison_index: BTreeMap<String, usize>,
}

/// Errors that future parsing, normalization, and comparison steps may report.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "kind", content = "data"))]
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
        Parser::new_ext(&source, markdown_options(options)).into_offset_iter()
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
    compare_many(reference, core::slice::from_ref(alternative), options)
}

/// Compares one reference document against zero or more alternatives.
pub fn compare_many(
    reference: &Document,
    alternatives: &[Document],
    options: &Options,
) -> Result<ComparisonSet, Error> {
    let reference = normalize_document(reference, options)?;
    let reference_blocks = build_reference_blocks(&reference);
    let mut comparisons = Vec::with_capacity(alternatives.len());
    let mut comparison_index = BTreeMap::new();

    for (index, alternative) in alternatives.iter().enumerate() {
        let normalized = normalize_document(alternative, options)?;
        let comparison = build_comparison(&reference, &reference_blocks, &normalized, options);

        if let Some(alternative_id) = &comparison.alternative_id {
            comparison_index.insert(alternative_id.clone(), index);
        }

        comparisons.push(comparison);
    }

    Ok(ComparisonSet {
        reference,
        reference_blocks,
        comparisons,
        comparison_index,
    })
}

fn build_comparison(
    reference: &NormalizedDocument,
    reference_blocks: &[ReferenceBlock],
    alternative: &NormalizedDocument,
    options: &Options,
) -> Comparison {
    let alternative_blocks = build_blocks(alternative);
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
                } else {
                    if let Some(pending) = pending.take() {
                        substitutions.push(pending.finish(reference, alternative));
                    }
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

    let (mut changed_regions, unchanged_regions) = build_block_regions(
        reference,
        reference_blocks,
        alternative,
        &alternative_blocks,
    );

    merge_adjacent_changed_regions(&mut changed_regions, options.merge_adjacent_regions_up_to);

    if options.block_level_changes_only {
        expand_changed_regions_to_blocks(&mut changed_regions, reference, reference_blocks);
        substitutions.clear();
    }

    Comparison {
        alternative_id: alternative.document_id.clone(),
        alternative_blocks,
        substitutions,
        changed_regions,
        unchanged_regions,
    }
}

fn has_following_change(ops: &[similar::DiffOp]) -> bool {
    ops.iter().any(|op| op.tag() != DiffTag::Equal)
}

fn markdown_options(options: &Options) -> MarkdownOptions {
    let mut markdown_options = MarkdownOptions::empty();

    if options.enable_extended_markdown {
        markdown_options.insert(MarkdownOptions::ENABLE_TABLES);
        markdown_options.insert(MarkdownOptions::ENABLE_FOOTNOTES);
        markdown_options.insert(MarkdownOptions::ENABLE_TASKLISTS);
        markdown_options.insert(MarkdownOptions::ENABLE_MATH);
    }

    markdown_options
}

fn build_reference_blocks(reference: &NormalizedDocument) -> Vec<ReferenceBlock> {
    build_blocks(reference)
}

fn build_blocks(document: &NormalizedDocument) -> Vec<ReferenceBlock> {
    let mut blocks = Vec::new();
    let mut open_blocks: Vec<OpenBlock> = Vec::new();
    let mut active_block_indices = Vec::new();
    let mut heading_path = Vec::new();
    let mut list_item_counters = Vec::new();

    for (index, token) in document.tokens.iter().enumerate() {
        match token {
            Token::Start(StructureKind::List { .. }) => list_item_counters.push(0),
            Token::End(StructureKind::List { .. }) => {
                list_item_counters.pop();
            }
            Token::Start(kind) if block_kind_for_structure(*kind).is_some() => {
                let block_kind = block_kind_for_structure(*kind).expect("checked");
                let block_index = blocks.len();
                let list_item_index = if matches!(kind, StructureKind::ListItem) {
                    list_item_counters.last_mut().map(|counter| {
                        *counter += 1;
                        *counter
                    })
                } else {
                    None
                };

                let effective_heading_path = if let StructureKind::Heading { level } = kind {
                    heading_path.truncate(level.saturating_sub(1) as usize);
                    heading_path.push(block_index);
                    heading_path.clone()
                } else {
                    heading_path.clone()
                };

                let anchor = BlockAnchor {
                    block_index,
                    block_path: active_block_indices
                        .iter()
                        .copied()
                        .chain(core::iter::once(block_index))
                        .collect(),
                    heading_path: effective_heading_path,
                    list_item_index,
                };

                open_blocks.push(OpenBlock {
                    kind: *kind,
                    token_start: index,
                    anchor: anchor.clone(),
                });
                active_block_indices.push(block_index);

                blocks.push(ReferenceBlock {
                    index: block_index,
                    kind: block_kind,
                    token_range: index..index,
                    source_range: 0..0,
                    anchor,
                    text: String::new(),
                });
            }
            Token::End(kind) if block_kind_for_structure(*kind).is_some() => {
                if let Some(position) = open_blocks.iter().rposition(|open| open.kind == *kind) {
                    let open = open_blocks.remove(position);
                    active_block_indices.pop();
                    let token_range = open.token_start..(index + 1);
                    let source_range =
                        source_span_for_token_range(&document.token_ranges, token_range.clone());
                    let text = visible_text_for_range(&document.tokens, token_range.clone());

                    if let Some(block) = blocks.get_mut(open.anchor.block_index) {
                        block.token_range = token_range;
                        block.source_range = source_range;
                        block.text = text;
                    }
                }
            }
            _ => {}
        }
    }

    blocks
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct BlockSignature {
    kind: BlockKind,
    heading_path: Vec<usize>,
    list_item_index: Option<usize>,
    text: String,
}

fn block_signature(block: &ReferenceBlock) -> BlockSignature {
    BlockSignature {
        kind: block.kind.clone(),
        heading_path: block.anchor.heading_path.clone(),
        list_item_index: block.anchor.list_item_index,
        text: block.text.clone(),
    }
}

fn block_kind_for_structure(kind: StructureKind) -> Option<BlockKind> {
    match kind {
        StructureKind::Paragraph => Some(BlockKind::Paragraph),
        StructureKind::Heading { .. } => Some(BlockKind::Heading),
        StructureKind::BlockQuote => Some(BlockKind::BlockQuote),
        StructureKind::ListItem => Some(BlockKind::ListItem),
        StructureKind::CodeBlock => Some(BlockKind::CodeBlock),
        StructureKind::TableRow => Some(BlockKind::TableRow),
        StructureKind::HtmlBlock => Some(BlockKind::HtmlBlock),
        StructureKind::FootnoteDefinition => Some(BlockKind::FootnoteDefinition),
        _ => None,
    }
}

fn visible_text_for_range(tokens: &[Token], token_range: Range<usize>) -> String {
    let mut text = String::new();

    for token in &tokens[token_range] {
        match token {
            Token::Text(value) => text.push_str(value),
            Token::Atom(AtomKind::InlineCode(value))
            | Token::Atom(AtomKind::InlineHtml(value))
            | Token::Atom(AtomKind::Html(value))
            | Token::Atom(AtomKind::FootnoteReference(value))
            | Token::Atom(AtomKind::InlineMath(value))
            | Token::Atom(AtomKind::DisplayMath(value)) => text.push_str(value),
            Token::Atom(AtomKind::SoftBreak) | Token::Atom(AtomKind::HardBreak) => text.push('\n'),
            _ => {}
        }
    }

    text
}

fn build_block_regions(
    reference: &NormalizedDocument,
    reference_blocks: &[ReferenceBlock],
    alternative: &NormalizedDocument,
    alternative_blocks: &[ReferenceBlock],
) -> (Vec<ChangeRegion>, Vec<UnchangedRegion>) {
    let reference_signatures: Vec<_> = reference_blocks.iter().map(block_signature).collect();
    let alternative_signatures: Vec<_> = alternative_blocks.iter().map(block_signature).collect();
    let ops = capture_diff_slices(
        Algorithm::Myers,
        &reference_signatures,
        &alternative_signatures,
    );
    let mut changed_regions = Vec::new();
    let mut unchanged_regions = Vec::new();

    for op in ops {
        match op.tag() {
            DiffTag::Equal => {
                if let Some(region) =
                    unchanged_region_for_block_range(op.old_range(), reference, reference_blocks)
                {
                    unchanged_regions.push(region);
                }
            }
            DiffTag::Delete | DiffTag::Insert | DiffTag::Replace => {
                changed_regions.push(change_region_for_block_ranges(
                    op.old_range(),
                    op.new_range(),
                    reference,
                    reference_blocks,
                    alternative,
                    alternative_blocks,
                ));
            }
        }
    }

    (changed_regions, unchanged_regions)
}

fn change_region_for_block_ranges(
    reference_block_range: Range<usize>,
    alternative_block_range: Range<usize>,
    reference: &NormalizedDocument,
    reference_blocks: &[ReferenceBlock],
    alternative: &NormalizedDocument,
    alternative_blocks: &[ReferenceBlock],
) -> ChangeRegion {
    let reference_range =
        token_range_for_blocks(reference_blocks, reference_block_range.clone()).unwrap_or(0..0);
    let alternative_range =
        token_range_for_blocks(alternative_blocks, alternative_block_range.clone()).unwrap_or(0..0);
    let block_indices: Vec<_> = reference_block_range.clone().collect();
    let block_kinds = block_indices
        .iter()
        .filter_map(|index| reference_blocks.get(*index))
        .map(|block| block.kind.clone())
        .collect();
    let primary_anchor = reference_block_range
        .start
        .checked_sub(0)
        .and_then(|index| reference_blocks.get(index))
        .map(|block| block.anchor.clone());
    let replacement = if alternative_range.is_empty() {
        Vec::new()
    } else {
        alternative.tokens[alternative_range.clone()].to_vec()
    };

    ChangeRegion {
        reference_source_range: source_span_for_token_range(
            &reference.token_ranges,
            reference_range.clone(),
        ),
        alternative_source_range: source_span_for_token_range(
            &alternative.token_ranges,
            alternative_range.clone(),
        ),
        weight: change_weight_for_block_ranges(
            &reference_block_range,
            &alternative_block_range,
            reference_blocks,
            alternative_blocks,
        ),
        reference_range,
        block_indices,
        block_kinds,
        primary_anchor,
        replacement,
    }
}

fn unchanged_region_for_block_range(
    reference_block_range: Range<usize>,
    reference: &NormalizedDocument,
    reference_blocks: &[ReferenceBlock],
) -> Option<UnchangedRegion> {
    let reference_range = token_range_for_blocks(reference_blocks, reference_block_range.clone())?;
    let block_indices: Vec<_> = reference_block_range.collect();
    let block_kinds = block_indices
        .iter()
        .filter_map(|index| reference_blocks.get(*index))
        .map(|block| block.kind.clone())
        .collect();
    let primary_anchor = block_indices
        .first()
        .and_then(|index| reference_blocks.get(*index))
        .map(|block| block.anchor.clone());

    Some(UnchangedRegion {
        reference_source_range: source_span_for_token_range(
            &reference.token_ranges,
            reference_range.clone(),
        ),
        reference_range,
        block_indices,
        block_kinds,
        primary_anchor,
    })
}

fn token_range_for_blocks(
    blocks: &[ReferenceBlock],
    block_range: Range<usize>,
) -> Option<Range<usize>> {
    if block_range.is_empty() {
        if block_range.start == 0 {
            return Some(0..0);
        }

        let previous = blocks.get(block_range.start.saturating_sub(1))?;
        return Some(previous.token_range.end..previous.token_range.end);
    }

    let first = blocks.get(block_range.start)?;
    let last = blocks.get(block_range.end.saturating_sub(1))?;

    Some(first.token_range.start..last.token_range.end)
}

fn change_weight_for_range(reference_range: &Range<usize>, replacement: &[Token]) -> ChangeWeight {
    let size = reference_range.len().max(replacement.len());

    if size <= 3 {
        ChangeWeight::Small
    } else if size <= 12 {
        ChangeWeight::Medium
    } else {
        ChangeWeight::Large
    }
}

fn change_weight_for_block_ranges(
    reference_block_range: &Range<usize>,
    alternative_block_range: &Range<usize>,
    reference_blocks: &[ReferenceBlock],
    alternative_blocks: &[ReferenceBlock],
) -> ChangeWeight {
    let reference_count = reference_block_range.len();
    let alternative_count = alternative_block_range.len();
    let block_span = reference_count.max(alternative_count);

    if block_span == 1 {
        let reference_text = reference_blocks
            .get(reference_block_range.start)
            .map(|block| block.text.as_str())
            .unwrap_or("");
        let alternative_text = alternative_blocks
            .get(alternative_block_range.start)
            .map(|block| block.text.as_str())
            .unwrap_or("");
        let char_delta = reference_text.len().abs_diff(alternative_text.len());

        if char_delta <= 16 {
            ChangeWeight::Small
        } else {
            ChangeWeight::Medium
        }
    } else if block_span <= 2 {
        ChangeWeight::Medium
    } else {
        ChangeWeight::Large
    }
}

#[allow(clippy::ptr_arg)]
fn merge_adjacent_changed_regions(changed_regions: &mut Vec<ChangeRegion>, merge_gap: usize) {
    if changed_regions.is_empty() {
        return;
    }

    let mut merged = Vec::with_capacity(changed_regions.len());
    let mut current = changed_regions[0].clone();

    for next in changed_regions.iter().skip(1) {
        if next
            .reference_range
            .start
            .saturating_sub(current.reference_range.end)
            <= merge_gap
        {
            current.reference_range.end = next.reference_range.end;
            current.reference_source_range.end = next.reference_source_range.end;
            current.alternative_source_range.end = next.alternative_source_range.end;
            current.replacement.extend(next.replacement.clone());
            current
                .block_indices
                .extend(next.block_indices.iter().copied());
            current.block_indices.sort_unstable();
            current.block_indices.dedup();
            current.block_kinds.extend(next.block_kinds.clone());
            current.block_kinds.dedup();
            current.weight =
                change_weight_for_range(&current.reference_range, &current.replacement);
            if current.primary_anchor.is_none() {
                current.primary_anchor = next.primary_anchor.clone();
            }
        } else {
            merged.push(current);
            current = next.clone();
        }
    }

    merged.push(current);
    *changed_regions = merged;
}

fn expand_changed_regions_to_blocks(
    changed_regions: &mut [ChangeRegion],
    reference: &NormalizedDocument,
    reference_blocks: &[ReferenceBlock],
) {
    for region in changed_regions.iter_mut() {
        if region.block_indices.is_empty() {
            continue;
        }

        let first = region.block_indices[0];
        let last = *region.block_indices.last().expect("not empty");
        if let (Some(first_block), Some(last_block)) =
            (reference_blocks.get(first), reference_blocks.get(last))
        {
            region.reference_range = first_block.token_range.start..last_block.token_range.end;
            region.reference_source_range = source_span_for_token_range(
                &reference.token_ranges,
                region.reference_range.clone(),
            );
            region.block_kinds = region
                .block_indices
                .iter()
                .filter_map(|index| reference_blocks.get(*index))
                .map(|block| block.kind.clone())
                .collect();
            region.primary_anchor = Some(first_block.anchor.clone());
            region.weight = change_weight_for_range(&region.reference_range, &region.replacement);
        }
    }
}

#[derive(Debug, Clone)]
struct OpenBlock {
    kind: StructureKind,
    token_start: usize,
    anchor: BlockAnchor,
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
        Event::InlineMath(text) => {
            push_token(
                tokens,
                token_ranges,
                Token::Atom(AtomKind::InlineMath(text.into_string())),
                source_range,
            );
            Ok(())
        }
        Event::DisplayMath(text) => {
            push_token(
                tokens,
                token_ranges,
                Token::Atom(AtomKind::DisplayMath(text.into_string())),
                source_range,
            );
            Ok(())
        }
        Event::Html(html) => {
            push_token(
                tokens,
                token_ranges,
                Token::Atom(AtomKind::Html(html.into_string())),
                source_range,
            );
            Ok(())
        }
        Event::InlineHtml(html) => {
            push_token(
                tokens,
                token_ranges,
                Token::Atom(AtomKind::InlineHtml(html.into_string())),
                source_range,
            );
            Ok(())
        }
        Event::FootnoteReference(label) => {
            push_token(
                tokens,
                token_ranges,
                Token::Atom(AtomKind::FootnoteReference(label.into_string())),
                source_range,
            );
            Ok(())
        }
        Event::TaskListMarker(checked) => {
            push_token(
                tokens,
                token_ranges,
                Token::Atom(AtomKind::TaskListMarker(checked)),
                source_range,
            );
            Ok(())
        }
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
        Tag::HtmlBlock => push_token(
            tokens,
            token_ranges,
            Token::Start(StructureKind::HtmlBlock),
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
        Tag::Strikethrough => push_token(
            tokens,
            token_ranges,
            Token::Start(StructureKind::Strikethrough),
            source_range,
        ),
        Tag::Strong => push_token(
            tokens,
            token_ranges,
            Token::Start(StructureKind::Strong),
            source_range,
        ),
        Tag::FootnoteDefinition(_) => push_token(
            tokens,
            token_ranges,
            Token::Start(StructureKind::FootnoteDefinition),
            source_range,
        ),
        Tag::Table(alignments) => {
            push_token(
                tokens,
                token_ranges,
                Token::Start(StructureKind::Table),
                source_range.clone(),
            );
            push_token(
                tokens,
                token_ranges,
                Token::Atom(AtomKind::TableAlignment(
                    alignments.into_iter().map(table_alignment_kind).collect(),
                )),
                source_range,
            );
        }
        Tag::TableHead => push_token(
            tokens,
            token_ranges,
            Token::Start(StructureKind::TableHead),
            source_range,
        ),
        Tag::TableRow => push_token(
            tokens,
            token_ranges,
            Token::Start(StructureKind::TableRow),
            source_range,
        ),
        Tag::TableCell => push_token(
            tokens,
            token_ranges,
            Token::Start(StructureKind::TableCell),
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
        TagEnd::HtmlBlock => push_token(
            tokens,
            token_ranges,
            Token::End(StructureKind::HtmlBlock),
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
        TagEnd::Strikethrough => push_token(
            tokens,
            token_ranges,
            Token::End(StructureKind::Strikethrough),
            source_range,
        ),
        TagEnd::Strong => push_token(
            tokens,
            token_ranges,
            Token::End(StructureKind::Strong),
            source_range,
        ),
        TagEnd::FootnoteDefinition => push_token(
            tokens,
            token_ranges,
            Token::End(StructureKind::FootnoteDefinition),
            source_range,
        ),
        TagEnd::Table => push_token(
            tokens,
            token_ranges,
            Token::End(StructureKind::Table),
            source_range,
        ),
        TagEnd::TableHead => push_token(
            tokens,
            token_ranges,
            Token::End(StructureKind::TableHead),
            source_range,
        ),
        TagEnd::TableRow => push_token(
            tokens,
            token_ranges,
            Token::End(StructureKind::TableRow),
            source_range,
        ),
        TagEnd::TableCell => push_token(
            tokens,
            token_ranges,
            Token::End(StructureKind::TableCell),
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

fn table_alignment_kind(alignment: Alignment) -> TableAlignmentKind {
    match alignment {
        Alignment::None => TableAlignmentKind::None,
        Alignment::Left => TableAlignmentKind::Left,
        Alignment::Center => TableAlignmentKind::Center,
        Alignment::Right => TableAlignmentKind::Right,
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
    fn preserves_heading_level() {
        let options = Options::default();
        let normalized = normalize_document(&Document::new("### Title"), &options).unwrap();

        assert_eq!(
            normalized.tokens,
            vec![
                Token::Start(StructureKind::Heading { level: 3 }),
                Token::Text("Title".to_string()),
                Token::End(StructureKind::Heading { level: 3 }),
            ]
        );
    }

    #[test]
    fn preserves_nested_emphasis_structure() {
        let options = Options::default();
        let normalized = normalize_document(&Document::new("***bold italic***"), &options).unwrap();

        assert_eq!(
            normalized.tokens,
            vec![
                Token::Start(StructureKind::Paragraph),
                Token::Start(StructureKind::Emphasis),
                Token::Start(StructureKind::Strong),
                Token::Text("bold".to_string()),
                Token::Text(" ".to_string()),
                Token::Text("italic".to_string()),
                Token::End(StructureKind::Strong),
                Token::End(StructureKind::Emphasis),
                Token::End(StructureKind::Paragraph),
            ]
        );
    }

    #[test]
    fn preserves_ordered_and_unordered_list_kinds_by_default() {
        let options = Options::default();
        let bullet = normalize_document(&Document::new("- one\n"), &options).unwrap();
        let ordered = normalize_document(&Document::new("1. one\n"), &options).unwrap();

        assert!(
            bullet
                .tokens
                .contains(&Token::Start(StructureKind::List { ordered: false }))
        );
        assert!(
            ordered
                .tokens
                .contains(&Token::Start(StructureKind::List { ordered: true }))
        );
        assert_ne!(bullet.tokens, ordered.tokens);
    }

    #[test]
    fn preserves_inline_code() {
        let options = Options::default();
        let normalized = normalize_document(&Document::new("Use `code` here."), &options).unwrap();

        assert!(
            normalized
                .tokens
                .contains(&Token::Atom(AtomKind::InlineCode("code".to_string())))
        );
    }

    #[test]
    fn preserves_fenced_code_block_language() {
        let options = Options::default();
        let normalized =
            normalize_document(&Document::new("```rust\nfn main() {}\n```"), &options).unwrap();

        assert!(
            normalized
                .tokens
                .contains(&Token::Start(StructureKind::CodeBlock))
        );
        assert!(
            normalized
                .tokens
                .contains(&Token::Atom(AtomKind::CodeBlockLanguage(Some(
                    "rust".to_string()
                ))))
        );
    }

    #[test]
    fn preserves_soft_and_hard_breaks() {
        let options = Options::default();
        let soft = normalize_document(&Document::new("one\ntwo"), &options).unwrap();
        let hard = normalize_document(&Document::new("one  \ntwo"), &options).unwrap();

        assert!(soft.tokens.contains(&Token::Atom(AtomKind::SoftBreak)));
        assert!(hard.tokens.contains(&Token::Atom(AtomKind::HardBreak)));
    }

    #[test]
    fn degrades_inline_html_to_tokens() {
        let options = Options::default();
        let normalized = normalize_document(&Document::new("<span>html</span>"), &options).unwrap();

        assert!(
            normalized
                .tokens
                .contains(&Token::Atom(AtomKind::InlineHtml("<span>".to_string())))
                || normalized.tokens.contains(&Token::Atom(AtomKind::Html(
                    "<span>html</span>".to_string()
                )))
        );
    }

    #[test]
    fn parses_task_lists_footnotes_tables_and_math() {
        let options = Options::default();

        let task_list = normalize_document(&Document::new("- [x] done"), &options).unwrap();
        assert!(
            task_list
                .tokens
                .contains(&Token::Atom(AtomKind::TaskListMarker(true)))
        );

        let footnote = normalize_document(&Document::new("[^1]\n\n[^1]: note"), &options).unwrap();
        assert!(
            footnote
                .tokens
                .contains(&Token::Atom(AtomKind::FootnoteReference("1".to_string())))
        );
        assert!(
            footnote
                .tokens
                .contains(&Token::Start(StructureKind::FootnoteDefinition))
        );

        let table = normalize_document(&Document::new("| a |\n| - |\n| b |\n"), &options).unwrap();
        assert!(table.tokens.contains(&Token::Start(StructureKind::Table)));
        assert!(
            table
                .tokens
                .contains(&Token::Start(StructureKind::TableRow))
        );

        let math = normalize_document(&Document::new("Inline $x$ and $$y$$"), &options).unwrap();
        assert!(math.tokens.iter().any(|token| matches!(
            token,
            Token::Atom(AtomKind::InlineMath(_)) | Token::Atom(AtomKind::DisplayMath(_))
        )));
    }

    #[test]
    fn compare_pair_returns_empty_substitutions_for_identical_normalization() {
        let options = Options::default();
        let reference = Document::new("Hello *world*.");
        let alternative = Document::new("Hello _world_.");

        let result = compare_pair(&reference, &alternative, &options).unwrap();

        assert_eq!(result.comparisons.len(), 1);
        assert!(result.comparisons[0].substitutions.is_empty());
        assert!(result.comparisons[0].changed_regions.is_empty());
        assert!(!result.comparisons[0].unchanged_regions.is_empty());
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
        assert_eq!(result.comparisons[0].changed_regions.len(), 1);
        assert_eq!(
            result.comparisons[0].changed_regions[0].weight,
            ChangeWeight::Small
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
        let reference_blocks = vec![ReferenceBlock {
            index: 0,
            kind: BlockKind::Paragraph,
            token_range: 0..4,
            source_range: 0..4,
            anchor: BlockAnchor {
                block_index: 0,
                block_path: vec![0],
                heading_path: vec![],
                list_item_index: None,
            },
            text: "abcd".to_string(),
        }];

        assert_eq!(
            build_comparison(&reference, &reference_blocks, &alternative, &options).substitutions,
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
    fn compare_many_builds_reference_blocks_and_id_index() {
        let options = Options::default();
        let reference = Document::with_id("reference", "# Title\n\nHello world.\n");
        let alternatives = vec![
            Document::with_id("a", "# Title\n\nHello there.\n"),
            Document::with_id("b", "# Other\n\nHello world.\n"),
        ];
        let result = compare_many(&reference, &alternatives, &options).unwrap();

        assert_eq!(result.reference_blocks.len(), 2);
        assert_eq!(result.reference_blocks[0].kind, BlockKind::Heading);
        assert_eq!(result.reference_blocks[1].kind, BlockKind::Paragraph);
        assert!(result.comparison_by_id("a").is_some());
        assert_eq!(result.comparison_index.get("b"), Some(&1));
    }

    #[test]
    fn block_level_changes_can_be_requested() {
        let options = Options {
            block_level_changes_only: true,
            ..Options::default()
        };
        let result = compare_pair(
            &Document::new("First paragraph.\n\nSecond paragraph.\n"),
            &Document::new("First paragraph.\n\nChanged paragraph.\n"),
            &options,
        )
        .unwrap();

        assert!(result.comparisons[0].substitutions.is_empty());
        assert_eq!(result.comparisons[0].changed_regions.len(), 1);
        assert_eq!(
            result.comparisons[0].changed_regions[0].block_kinds,
            vec![BlockKind::Paragraph]
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
    #[cfg(feature = "serde")]
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
    #[cfg(feature = "serde")]
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
