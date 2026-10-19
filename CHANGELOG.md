# Changelog

## 0.2.1

- Preserve unchanged paragraphs when blocks are inserted before or between them.
- Compare nested block subtrees without duplicating replacement tokens.
- Keep overlapping multi-alternative replacements intact and include unchanged
  content and interior insertions when building a shared region.
- Sort variant clusters in reference order, including insertion boundaries.
- Preserve intervening structure tokens when merging adjacent change regions.
- Keep decoded text source ranges valid and round source-map offsets down to
  UTF-8 character boundaries.
- Update compatible dependencies, including similar 3.2.0.
- Modernize CI, add weekly Dependabot checks, and correct API documentation.

Validated with Rust 1.99.1. The public Rust data model is unchanged.
