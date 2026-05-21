# markalign

`markalign` is a Rust library crate for comparing Markup documents at the syntax level instead of as raw source text.

The immediate target is Markdown. The long-term goal is to support additional Markup languages and, where useful, multiple parser backends.

## Why

Raw-text diffs are often a poor fit for Markup documents:

- Different source forms can render to the same document.
- Small source edits can produce large structural changes.
- Formatting syntax can distract from the actual textual or structural change.

`markalign` aims to compare documents after parsing, so the result reflects the Markup structure that a reader or renderer actually sees.

## Scope

The library accepts one reference document and zero or more alternative documents.

- With one input document, `markalign` can be used for validation, parsing, and normalization.
- With multiple input documents, `markalign` compares each alternative against the reference.

A document is a stream of Unicode characters representing Markup source.

## Pipeline

The current implementation follows this pipeline:

1. Apply optional pre-parse normalization such as Unicode normalization.
2. Parse each source document with a Markdown parser.
3. Convert each parsed document into a normalized token stream.
4. Diff each alternative token stream against the reference token stream.
5. Consolidate the raw diff into higher-level substitutions.
6. Serialize or render the result into downstream formats.

## Version 1

The first implementation is intentionally narrow.

- Target only Markdown.
- Support exactly one parser backend: [`pulldown-cmark`](https://docs.rs/crate/pulldown-cmark/latest).
- Produce a stable normalized token stream.
- Compare one reference against one or more alternatives.
- Return substitutions that describe how to transform the reference into each alternative.

Anything beyond that should be treated as a later extension, not as part of the initial build.

## API

The main entry points are:

- `normalize_document`: parse one Markdown document and return its normalized token stream.
- `compare_pair`: compare one reference document against one alternative.
- `compare_many`: compare one reference document against zero or more alternatives.

Example:

```rust
use markalign::{Document, Options, compare_pair, normalize_document};

let options = Options::default();
let reference = Document::with_id("reference", "Hello *world*.");
let alternative = Document::with_id("alternative", "Hello _world_.");

let normalized = normalize_document(&reference, &options)?;
let comparison = compare_pair(&reference, &alternative, &options)?;

assert!(!normalized.tokens.is_empty());
assert!(comparison.comparisons[0].substitutions.is_empty());
# Ok::<(), markalign::Error>(())
```

## Core Model

The public API is built around these core types:

- `Document`: one parsed and normalized source document.
- `Token`: one unit in the normalized syntax stream.
- `Diff`: a token-level comparison between a reference and an alternative.
- `Substitution`: one contiguous replacement of part of the reference with part of an alternative.
- `Comparison`: the full result for one reference and one alternative.
- `ComparisonSet`: the full result for comparing one reference against all alternatives.
- `SourceSpan`: a byte range plus line and column positions.

For `v1`, tokenization should favor determinism over richness. If a syntax detail is not needed for comparison, it should be normalized away instead of preserved by default.

## Tokenization

The working idea is:

- Visible text becomes text tokens split into word, whitespace, and punctuation chunks.
- Structural or formatting boundaries become explicit tokens.
- A formatting region can be represented by matching start and end tokens around the enclosed content.

For example, emphasis, strong emphasis, links, headings, paragraphs, and list items can all be represented by structure tokens plus text tokens.

For `v1`, the parser backend is `pulldown-cmark`, and the normalized token model is intentionally narrower than the full parser event space. Unsupported Markdown-adjacent features such as inline HTML are currently rejected instead of being normalized silently.

## Diff And Consolidation

Each alternative token stream is compared to the reference token stream. The current implementation uses the [`similar`](https://docs.rs/crate/similar/latest) crate with a token-sequence diff.

The raw diff is then consolidated:

- Adjacent removals and additions become a single substitution.
- Adjacent text tokens can be merged into larger text spans.
- Small accidental equalities can be absorbed into nearby substitutions when that produces a more meaningful result.
- Markup features that are intentionally ignored by the chosen normalization can be removed before or during consolidation.

The result should prefer human-meaningful edits over mechanically minimal token edits.

In the current `v1` implementation, consolidation is still intentionally simple: it groups contiguous replace, insert, and delete regions into substitutions, and it can absorb small equal token runs between surrounding changes.

Text diffs are finer than whole parser text events: visible text is split into word, whitespace, and punctuation chunks before diffing. This keeps common edits such as `Hello world.` to `Hello there.` localized to the changed word.

## Markdown Support

`v1` deliberately supports a small Markdown subset.

| Feature | Status | Notes |
| --- | --- | --- |
| Paragraphs | Supported | Represented as start and end structure tokens. |
| Headings | Supported | Heading level is preserved. |
| Emphasis and strong emphasis | Supported | Source spelling such as `*` vs `_` is normalized away. |
| Links and images | Supported | Destination and title are preserved as atom tokens. |
| Ordered and unordered lists | Supported | List kind is preserved by default and can be normalized with `equate_list_kinds`. |
| Inline code | Supported | Preserved as an atom token. |
| Code blocks | Partial | Code block structure and fenced language are preserved. |
| Thematic breaks | Supported | Preserved as an atom token. |
| Soft and hard breaks | Supported | Preserved as atom tokens. |
| Inline HTML and block HTML | Rejected | Returns `UnsupportedFeature`. |
| Footnotes | Rejected | Returns `UnsupportedFeature`. |
| Math | Rejected | Returns `UnsupportedFeature`. |
| Task lists | Rejected | Returns `UnsupportedFeature`. |

## Output

The final result should make two things available:

- A normalized representation of the reference document.
- For each alternative, a list of substitutions that would transform the reference into that alternative.
- Source ranges for normalized tokens and substitutions, so downstream tooling can map results back to the original Markdown.
- A source-map-style position model that can convert byte ranges into line and column spans.
- Serde-compatible data structures for serializing comparison results.

Examples:

- If an alternative is identical to the reference, the substitution list is empty.
- If an alternative is completely different, the result may be a single substitution replacing the whole reference.
- If an alternative differs only in a few local edits, the result should be a short list of local substitutions.

The result should also be serializable.

## Non-Goals For Version 1

To keep the first implementation tractable, `v1` should not try to solve all Markdown edge cases at once.

- No support for multiple Markup languages.
- No support for multiple parser backends.
- No attempt to preserve every source-level formatting choice.
- No guarantee yet that all Markdown constructs will receive specialized token types.

## Open Design Questions

The following questions should be resolved before implementation gets deep:

- Which source differences should normalize to the same token stream?
- How should links, code spans, HTML-in-Markdown, and reference-style constructs be represented?
- When should small equal regions be merged into a surrounding substitution?
- Which serialized fields should be guaranteed as long-term stable API?

## Worked Examples

These examples are intentionally schematic. They describe the behavior, not the final wire format.

### Example 1: Equivalent Markdown Source

Reference:

```md
Hello *world*.
```

Alternative:

```md
Hello _world_.
```

Expected outcome:

- Both documents normalize to the same token stream.
- The substitution list is empty.

### Example 2: Local Text Edit

Reference:

```md
Hello *world*.
```

Alternative:

```md
Hello *there*.
```

Expected outcome:

- The surrounding emphasis structure remains aligned.
- One substitution replaces `world` with `there`.

### Example 3: Structural Change

Reference:

```md
- one
- two
```

Alternative:

```md
1. one
2. two
```

Expected outcome:

- If unordered and ordered lists are treated as meaningfully different in `v1`, this becomes a structural substitution.
- If the chosen normalization intentionally erases that distinction, the substitution list is empty.

This kind of case should be decided explicitly by the token model, not left accidental.
