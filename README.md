# markalign

`markalign` is a planned Rust library crate to parse and compare two or more Markup source documents based on parsed Markup syntax.

This is more useful than comparing raw sourc documents, because often differences in sources make no difference for Markup, and often small changes in sources lead to big changes for Markup.

As a start, we will target MarkDown, using a popular suitable MarkDown parser. We hope to support other Markup languages or other parsers in the future.

The user will provide multiple Markup source document, of which one is the reference and the otehrs are alternatives as a reference, and one of more Markup documents s alternatives. Well, it is also possible to provide just one document, whichc means one reference and no alternatives, which is useful if you only want validation and parsing.

A source document is a stream of Unicode characters that represents valid Markup. 

There may be some pre-parse filtering such as UniCode normalization.

A parser will parse the source document and `markalign` will use the result to create a stream of tokens. Every character to be displayed will become a character token. All structuring and formatting will be represented by format tokens. If a structuring or formatting applies to a portion of text, it will be represented by a start and an end token, enclosing the text to which it applies.

Then, each alternative token stream will be compared to the reference token stream using the [similar crate](https://crates.io/crates/similar) to get a diff stream of tokens that are equal, tokes that are removed and tokens that are added.

Then, the diff stream is consolidated. Consecutive remove and add tokens are consolidated into a single substitution. Consecutive character tokens are consolidated into a single string token.

Consolidation can also remove spurious equalities, that is, small pieces of text that happen to occurr in both the reference and one of the alternatives, but are probably insignificant, will be merged into substitutions.

Consolidation can also be used to remove undesired Markup features.

In the end, we have a stream of tokens representing the compiled reference, which can easily be converted to, for example, HTML. 

It also contains tokens that mark the begining and end of each piece of the reference that would need to be replaced to turn the reference into one of the alternatives. IN other words, each alternative is represented as a list of substitions that would need to be applied to the reference. For example:

 * If an alternative is identical to the reference, the list of substitutions is empty.
 * If an alternative is completely different from the reference, it will be one substitution, which consists of replacing the entire reference with the entire alternative.
 * If the alternative is essentially the reference with some minor edits, then it is a list of substitutions reflecting these edits

`markalign` will also support serialization of the result.
