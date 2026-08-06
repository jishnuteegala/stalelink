# Parse Benchmark Receipts

These are language-decision receipts for brief decision 18. They compare local
document parsing and link extraction only, not walking, checking, reporting, or
fixing. `bench/run.ps1` refuses to write results unless both implementations
emit the same sorted structured records: document, decoded URL, format-specific
location, and byte span for text formats.

## Results

Measured on 2026-08-06. Values are replaced from `bench/results/results.json`
after nine outer process invocations; the parenthesized range is min-max.

| Implementation | Aggregate docs/sec median (min-max) | In-process peak working set (MiB) | Cold completion median ms (min-max) | Cold peak working set (MiB) |
| --- | ---: | ---: | ---: | ---: |
| Rust production extraction API | 199.02 (195.65-203.19) | 225.02 | 2,110 (2,079-2,159) | 224.01 |
| Go throwaway port | 90.62 (88.65-92.95) | 146.78 | 3,782 (3,679-4,009) | 122.65 |

| Format | Rust docs/sec median (min-max) | Go docs/sec median (min-max) |
| --- | ---: | ---: |
| Markdown | 236.87 (227.07-240.99) | 90.87 (87.18-91.42) |
| HTML | 215.77 (194.50-217.08) | 83.10 (80.36-84.38) |
| Plain text | 246.19 (241.66-250.18) | 83.82 (82.94-85.42) |
| DOCX | 250.06 (240.95-255.88) | 148.82 (143.56-152.14) |
| XLSX | 246.69 (239.51-249.61) | 137.43 (130.50-138.83) |
| PPTX | 254.70 (245.14-259.83) | 136.40 (70.40-138.09) |
| PDF | 222.00 (216.85-225.54) | 86.55 (83.49-86.92) |

Host: Windows 11 Home 10.0.26200 (build 26200), Intel Core i7-11800H at
2.30 GHz (8 cores, 16 logical processors), 15.68 GiB RAM; rustc 1.89.0 and Go
1.26.5 (`bench/results/machine.json`, `rustc --version`, `go version`).

## Corpus

`bench/rust-harness` regenerates the corpus with a fixed `0x5EED_C0DE` PRNG
seed. It generates 315 documents, 45 per format, and validates 64,095 links:
9,150 each in Markdown, HTML, UTF-8 text, DOCX, XLSX, and PPTX, plus 9,195 in
PDF (including 45 annotation URIs). Each size tier has 15 independently seeded
copies with 10, 100, or 500 links. A Rust pass is approximately 1.5 seconds or
more under the reported steady-state method.

Templates vary surrounding prose and entities. Markdown covers inline links and
autolinks; HTML covers quoted and unquoted `href`/`src` attributes; plain text
covers prose URLs; DOCX, XLSX, and PPTX primarily cover relationship links;
and PDF covers text streams plus an annotation per annotated fixture. These are
synthetic templates, not a representative sample of arbitrary real documents.

## Method

Run `powershell.exe -NoProfile -File bench/run.ps1` from the repository root.
The runner rebuilds the corpus and binaries, requires every child exit code to
be zero, parses JSON receipts, and checks a canonical digest of every record
against the opposite implementation and the baseline on every aggregate trial.
Before any timing it independently generates a validation-only fixture set and
requires full structured-record equality. It also parity-checks the baseline,
cold trials, aggregate trials, and one baseline receipt for every format.

Cold completion is process launch through full-corpus extraction, JSON receipt
emission, and process exit. It uses 10 fresh-process trials in alternating Rust
and Go order, reporting the statistical median (the mean of the middle pair for
an even count). Throughput performs two warmup passes and seven timed extraction
passes per process; nine outer invocations provide the reported aggregate and
per-format medians and min-max ranges. Timed passes measure extraction only;
corpus generation, binary builds, process launch, JSON receipt handling, and
teardown are excluded from throughput timing.

On Windows, the runner retains the process handle and reads the kernel process
memory counter through `GetProcessMemoryInfo` after exit. The reported value is
`PeakWorkingSetSize`, labelled peak working set rather than RSS.

## Coverage

| Format | Rust production extractor | Go benchmark extractor | Structured comparison |
| --- | --- | --- | --- |
| Markdown | `pulldown-cmark` plus byte spans | Goldmark parser-owned definition segments and AST raw span mapping | URL, text location, raw span |
| HTML | `html5tokenizer` plus byte spans | `x/net/html` tokenizer | URL, text location, raw span |
| Plain text | `linkify` plus byte spans | HTTP(S) recognizer with explicit boundaries | URL, text location, raw span |
| DOCX | `zip` plus `quick-xml` | `archive/zip` plus streaming `encoding/xml` | URL and paragraph location |
| XLSX | `zip` plus `quick-xml` | workbook/sheet relationships plus streaming XML | URL and sheet/cell location |
| PPTX | `zip` plus `quick-xml` | presentation-order slide relationships plus streaming XML | URL and slide location |
| PDF | `lopdf` pages, annotations, and content streams | pdfcpu xref/page model, annotations, decoded content | URL, page, and annotation location |

## Coverage Delta

| Production behavior | Benchmark treatment |
| --- | --- |
| Markdown reference definitions and escaped destinations; HTML repeated, entity-decoded, and unquoted destinations | Validation-only fixtures assert raw spans and decoded URLs; unquoted HTML attributes are also timed. |
| DOCX field instructions, multi-sheet formulas, and multi-slide ordering | Validation-only fixtures assert these paths; timed OOXML templates primarily measure relationship links. |
| Multi-page PDFs with indirect annotations and FlateDecode content | Validation-only fixtures assert these paths; timed PDFs are simpler synthetic streams and annotations. |
| PDF encryption and non-URI action variants | Excluded from the measured workload on both implementations. |

The Go port remains benchmark-only and is not a product implementation.
