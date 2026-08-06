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
| Rust production extraction API | 204.22 (184.75-206.70) | 225.02 | 2,124 (1,988-2,556) | 223.87 |
| Go throwaway port | 91.31 (80.05-93.76) | 126.54 | 3,877 (3,659-4,162) | 137.60 |

| Format | Rust docs/sec median (min-max) | Go docs/sec median (min-max) |
| --- | ---: | ---: |
| Markdown | 240.42 (232.73-245.93) | 89.70 (78.53-92.35) |
| HTML | 203.14 (181.19-226.70) | 76.27 (70.88-82.09) |
| Plain text | 226.08 (191.99-239.66) | 83.25 (81.49-84.29) |
| DOCX | 241.19 (207.64-247.83) | 134.45 (124.47-140.80) |
| XLSX | 226.42 (198.79-243.55) | 128.65 (116.16-134.54) |
| PPTX | 242.87 (227.17-255.58) | 134.31 (123.96-137.19) |
| PDF | 223.26 (195.36-227.21) | 86.00 (84.07-87.39) |

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
| Markdown | `pulldown-cmark` plus byte spans | Goldmark AST plus raw span mapping | URL, text location, raw span |
| HTML | `html5tokenizer` plus byte spans | `x/net/html` tokenizer | URL, text location, raw span |
| Plain text | `linkify` plus byte spans | HTTP(S) recognizer with explicit boundaries | URL, text location, raw span |
| DOCX | `zip` plus `quick-xml` | `archive/zip` plus streaming `encoding/xml` | URL and paragraph location |
| XLSX | `zip` plus `quick-xml` | workbook/sheet relationships plus streaming XML | URL and sheet/cell location |
| PPTX | `zip` plus `quick-xml` | presentation-order slide relationships plus streaming XML | URL and slide location |
| PDF | `lopdf` pages, annotations, and content streams | pdfcpu xref/page model, annotations, decoded content | URL, page, and annotation location |

## Coverage Delta

| Production behavior | Benchmark treatment |
| --- | --- |
| Markdown reference definitions and escaped destinations; HTML repeated and entity-decoded destinations | Validation-only fixtures assert raw spans and decoded URLs; excluded from timed synthetic templates. |
| DOCX field instructions, multi-sheet formulas, and multi-slide ordering | Validation-only fixtures assert these paths; timed OOXML templates primarily measure relationship links. |
| Multi-page PDFs with indirect annotations and FlateDecode content | Validation-only fixtures assert these paths; timed PDFs are simpler synthetic streams and annotations. |
| PDF encryption and non-URI action variants | Excluded from the measured workload on both implementations. |

The Go port remains benchmark-only and is not a product implementation.
