use markalign::{ComparisonSet, Document, Options, Token, compare_many, normalize_document};

fn assert_clusters_reconstruct(result: &ComparisonSet, alternatives: &[Document]) {
    for (index, alternative) in alternatives.iter().enumerate() {
        let mut tokens = result.reference.tokens.clone();
        let edits: Vec<_> = result
            .variant_clusters
            .iter()
            .filter_map(|cluster| {
                cluster
                    .variants
                    .iter()
                    .find(|v| v.alternative_index == index)
                    .map(|variant| (cluster.reference_range.clone(), &variant.replacement))
            })
            .collect();
        for pair in edits.windows(2) {
            assert!(
                pair[0].0.end <= pair[1].0.start,
                "overlapping or unsorted clusters"
            );
        }
        for (range, replacement) in edits.into_iter().rev() {
            tokens.splice(range, replacement.clone());
        }
        let expected = normalize_document(alternative, &Options::default()).unwrap();
        assert_eq!(
            tokens, expected.tokens,
            "clusters must reconstruct alternative {index}"
        );
    }
}

#[test]
fn insertions_preserve_existing_paragraphs_and_boundaries() {
    let reference = Document::new("Alpha.\n\nBeta.\n");
    for (source, boundary) in [
        ("Inserted.\n\nAlpha.\n\nBeta.\n", 0),
        ("Alpha.\n\nInserted.\n\nBeta.\n", 4),
        ("Alpha.\n\nBeta.\n\nInserted.\n", 8),
    ] {
        let alternatives = [Document::with_id("a", source)];
        let result = compare_many(&reference, &alternatives, &Options::default()).unwrap();
        assert_eq!(result.variant_clusters.len(), 1);
        assert_eq!(
            result.variant_clusters[0].reference_range,
            boundary..boundary
        );
        assert_eq!(result.comparisons[0].unchanged_regions.len(), 2);
        assert_clusters_reconstruct(&result, &alternatives);
    }
}

#[test]
fn overlapping_changes_have_complete_nonduplicated_replacements() {
    let reference = Document::new("Alpha.\n\nBeta.\n\nGamma.\n");
    let alternatives = [
        Document::with_id("a", "Combined.\n\nGamma.\n"),
        Document::with_id("b", "Alpha.\n\nChanged.\n\nGamma.\n"),
        reference.clone(),
    ];
    let result = compare_many(&reference, &alternatives, &Options::default()).unwrap();
    assert_clusters_reconstruct(&result, &alternatives);
}

#[test]
fn insertion_inside_another_alternatives_replacement_is_not_lost() {
    let reference = Document::new("Alpha.\n\nBeta.\n\nGamma.\n");
    let alternatives = [
        Document::new("Combined.\n\nGamma.\n"),
        Document::new("Alpha.\n\nInserted.\n\nBeta.\n\nGamma.\n"),
    ];
    let result = compare_many(&reference, &alternatives, &Options::default()).unwrap();
    assert_clusters_reconstruct(&result, &alternatives);
}

#[test]
fn insertions_and_changes_are_reported_in_reference_order() {
    let alternatives = [Document::new("Inserted.\n\nAlpha.\n\nChanged.\n")];
    let result = compare_many(
        &Document::new("Alpha.\n\nBeta.\n"),
        &alternatives,
        &Options::default(),
    )
    .unwrap();
    assert_clusters_reconstruct(&result, &alternatives);
}

#[test]
fn joint_regions_reconstruct_combinations_of_insertions_deletions_and_replacements() {
    let reference = Document::new("Alpha.\n\nBeta.\n\nGamma.\n");
    let sources = [
        "Alpha.\n\nBeta.\n\nGamma.\n",
        "Alpha.\n\nGamma.\n",
        "Gamma.\n",
        "Alpha.\n",
        "Combined.\n\nGamma.\n",
        "Alpha.\n\nChanged.\n\nGamma.\n",
        "Alpha.\n\nInserted.\n\nBeta.\n\nGamma.\n",
        "Inserted.\n\nAlpha.\n\nBeta.\n\nGamma.\n",
        "Alpha.\n\nBeta.\n\nGamma.\n\nInserted.\n",
        "Changed.\n",
        "\n",
    ];
    for left in sources {
        for right in sources {
            let alternatives = [Document::with_id("a", left), Document::with_id("b", right)];
            let result = compare_many(&reference, &alternatives, &Options::default()).unwrap();
            assert_clusters_reconstruct(&result, &alternatives);
        }
    }
}

#[test]
fn nested_blocks_do_not_duplicate_replacement_tokens() {
    for (reference, alternative) in [
        ("> Alpha.\n>\n> Beta.\n", "> Changed.\n>\n> Other.\n"),
        ("- outer\n  - inner\n", "- changed\n  - nested\n"),
        (
            "- outer\n  - inner\n\nTail.\n",
            "- changed\n  - nested\n\nOther.\n",
        ),
    ] {
        let alternatives = [Document::new(alternative)];
        let result = compare_many(
            &Document::new(reference),
            &alternatives,
            &Options::default(),
        )
        .unwrap();
        assert_clusters_reconstruct(&result, &alternatives);
        let mut tokens = result.reference.tokens.clone();
        for region in result.comparisons[0].changed_regions.iter().rev() {
            tokens.splice(region.reference_range.clone(), region.replacement.clone());
        }
        assert_eq!(
            tokens,
            normalize_document(&alternatives[0], &Options::default())
                .unwrap()
                .tokens
        );
        let options = Options {
            block_level_changes_only: true,
            ..Options::default()
        };
        let block_result =
            compare_many(&Document::new(reference), &alternatives, &options).unwrap();
        let mut block_tokens = block_result.reference.tokens.clone();
        for region in block_result.comparisons[0].changed_regions.iter().rev() {
            block_tokens.splice(region.reference_range.clone(), region.replacement.clone());
        }
        assert_eq!(block_tokens, tokens);
    }
}

#[test]
fn source_ranges_remain_valid_for_decoded_text_and_unicode() {
    for source in [
        "&NotEqualTilde; é",
        "\\*é café",
        "&#x1F600; text",
        "a &amp; b",
    ] {
        let normalized = normalize_document(&Document::new(source), &Options::default()).unwrap();
        for (token, range) in normalized.tokens.iter().zip(&normalized.token_ranges) {
            let raw = normalized
                .source
                .get(range.clone())
                .expect("valid UTF-8 source range");
            if let Token::Text(text) = token {
                assert!(!raw.is_empty(), "text {text:?} needs a source span");
            }
        }
        normalized.token_spans();
    }
    let normalized = normalize_document(&Document::new("&#x1F600;"), &Options::default()).unwrap();
    let index = normalized
        .tokens
        .iter()
        .position(|token| matches!(token, Token::Text(_)))
        .unwrap();
    assert_eq!(
        &normalized.source[normalized.token_ranges[index].clone()],
        "&#x1F600;"
    );
}

#[test]
fn source_map_accepts_offsets_inside_multibyte_characters() {
    let map = markalign::SourceMap::new("é😀\ntext");
    for byte in 0..12 {
        let position = map.position_at(byte);
        assert!("é😀\ntext".is_char_boundary(position.byte));
    }
}
