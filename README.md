# markalign

The purpose of this crate is to compare Markup source documents based on Markup semantics using a Markup parser.

Simply comparing the source documents directly is not useful, because not all differences are relevant in Markup. We want to know how the compiled documents differ, not hwo the sources differ.

As a start, we will target MarkDown, using a popular suitable MarkDown parser. We may support other parsers of Markup languages in the future.

The user will provide a Markup source document as a reference, and one of more Markup documents s alternatives. Well, it is also possible to provide no alternatives, but that is less interesting.

A source document is a stream of Unicode characters that represents valid Markup. 

There may be some pre-parse filtering such as UniCode normalization.

A parser will parse the source document and we will use the result to create a stream of tokens. Every character to be displayed will become a character token. All structuring and formatting will be represented by format tokens. If a structuring or formatting applies to a portion of text, it will be represented by a start and an end token, enclosing the relevant text.

Then, each alternative token stream will be compared to the reference token stream using the [similar crate](https://crates.io/crates/similar) to get a diff stream of tokens that are equal, tokes that are removed and tokens that are added.

Then, the diff stream is consolidated. Consecutive remove and add tokens are consolidated into a single substitution. Consecutive character tokens are consolidated into strings.

In the end, we have a stream of tokens representing the compiled reference, which can easily be converted to, for example, HTML. It also contains tokens that mark the begining and end of each piece of the reference that would need to be replaced to turn it into each of the alternatives.
