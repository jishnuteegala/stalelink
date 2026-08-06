# THROWAWAY Go Port

This is a benchmark-only extraction port for the language-decision receipt. It
is not supported, released, or included in the Cargo workspace/cargo-dist plan.

It uses the Go standard library: `archive/zip` and regexp scans for OOXML
relationship targets, HTML `href` attributes, and URL-like text/PDF content.
It reports link counts, but deliberately does not implement stalelink's rich
locations or byte spans. See `docs/benchmarks.md` for the fidelity caveats.
