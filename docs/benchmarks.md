# Parse Benchmark Receipts

These are language-decision receipts for brief decision 18. They compare local
document parsing and link extraction only, not walking, checking, reporting, or
fixing. `bench/run.ps1` refuses to write results unless both implementations
emit the same sorted structured records: document, decoded URL, format-specific
location, and byte span for text formats.

## Results

Measured on 2026-08-06. Values are replaced from `bench/results/results.json`
nine in-process invocations; the parenthesized range is min-max.

| Implementation | Aggregate docs/sec median (min-max) | In-process peak working set (MiB) | Cold completion median ms (min-max) | Cold peak working set (MiB) |
| --- | ---: | ---: | ---: | ---: |
| Rust production extraction API | 207.31 (201.74-222.06) | 225.00 | 2,175 (1,958-2,521) | 224.01 |
| Go throwaway port | 82.31 (75.24-85.74) | 140.83 | 4,150 (3,863-4,601) | 137.75 |

| Format | Rust docs/sec median (min-max) | Go docs/sec median (min-max) |
| --- | ---: | ---: |
| Markdown | 241.79 (215.97-248.07) | 74.63 (64.70-77.10) |
| HTML | 224.48 (214.38-233.19) | 75.14 (72.05-76.61) |
| Plain text | 257.29 (239.03-263.46) | 75.52 (71.97-76.69) |
| DOCX | 259.28 (248.61-272.44) | 139.45 (115.46-148.44) |
| XLSX | 258.86 (216.22-278.05) | 131.19 (123.80-136.61) |
| PPTX | 274.18 (257.50-284.64) | 133.67 (131.61-137.19) |
| PDF | 236.58 (203.83-244.73) | 83.09 (72.87-85.68) |

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

Templates vary surrounding prose and entities; Markdown covers inline and

## Method

Run `powershell.exe -NoProfile -File bench/run.ps1` from the repository root.
The runner rebuilds the corpus and binaries, requires every child exit code to
be zero, parses JSON receipts, and checks a canonical digest of every record
against the opposite implementation and the baseline on every aggregate trial.

Cold completion is process launch through full-corpus extraction, JSON receipt
emission, and process exit. It uses 10 fresh-process trials in alternating Rust
and Go order, reporting the statistical median (the mean of the middle pair for
and seven timed extraction passes per process; nine invocations provide the
reported aggregate and per-format medians and min-max ranges.

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
| Markdown source-range reconstruction for exotic escaped/nested destinations | Not independently represented by this synthetic corpus; the receipt measures only records both implementations structurally validate. |
| OOXML field instructions, multi-sheet formulas, and multi-slide ordering | Go implements these paths, but this receipt's generated workload primarily measures relationship links; a future corpus revision should add independent multi-part fixtures before using this receipt for those subfeatures. |
| PDF encryption and non-URI action variants | Excluded from the measured workload on both implementations. |

The Go port remains benchmark-only and is not a product implementation.
