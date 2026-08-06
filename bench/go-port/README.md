# THROWAWAY Go Port

This is a benchmark-only extraction port for the language-decision receipt. It
is not supported, released, or included in the Cargo workspace/cargo-dist plan.

The port emits the same structured records that the receipt compares: decoded
URLs, format-specific locations, and byte spans for source-text formats.
Markdown uses Goldmark's AST; HTML uses `x/net/html`; OOXML uses `archive/zip`
and streaming `encoding/xml` for document parts and relationship resolution;
PDF uses pdfcpu's xref and page model for annotations and decoded page content.
Plain text uses an explicit HTTP(S) recognizer modelled on the production URL
boundary rules. See `docs/benchmarks.md` for scope and remaining coverage
limits.
