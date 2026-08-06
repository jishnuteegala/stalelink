# Parse Benchmark Receipts

These are language-decision receipts for brief decision 18. They compare local
document parsing and link extraction only, not walking, checking, reporting, or
fixing. `bench/run.ps1` refuses to write results unless both implementations
emit the same sorted structured records: document, decoded URL, format-specific
location, and byte span for text formats.

## Results

Measured on 2026-08-06. The values below are replaced from
`bench/results/results.json` after each full receipt refresh.

| Implementation | In-process median docs/sec | In-process peak working set (MiB) | Cold completion median ms (min-max) | Cold peak working set (MiB) |
| --- | ---: | ---: | ---: | ---: |
| Rust production extraction API | 219.75 | 82.43 | 679 (643-795) | 81.62 |
| Go throwaway port | 80.86 | 104.99 | 1,460 (1,409-1,581) | 99.49 |

## Corpus

`bench/rust-harness` regenerates the corpus with a fixed `0x5EED_C0DE` PRNG
seed. It generates 105 documents, 15 per format, and validates 21,365 links:
3,050 each in Markdown, HTML, UTF-8 text, DOCX, XLSX, and PPTX, plus 3,065 in
PDF (including 15 annotation URIs). Each size tier has five independently
seeded templates with 10, 100, or 500 links.

Templates vary surrounding prose and entities; Markdown covers inline,
autolink, and reference-definition syntax; HTML covers quoted, single-quoted,
and unquoted attributes; OOXML includes content parts plus ignored non-hyperlink
relationships; and PDFs include both plain and FlateDecode-compressed content
streams. The generator uses only tolerated inputs shared by both ports. It
does not currently generate non-UTF-8 text or malformed XML because production
Rust extraction rejects those inputs rather than recovering from them.

## Method

Run `powershell.exe -NoProfile -File bench/run.ps1` from the repository root.
The runner rebuilds the corpus and binaries, then captures stdout and stderr,
requires every child exit code to be zero, parses JSON receipts, and checks a
canonical digest of every record against the opposite implementation and the
baseline on every trial.

Cold completion is process launch through full-corpus extraction, JSON receipt
emission, and process exit. It uses 10 fresh-process trials in alternating Rust
and Go order, reporting a whole-millisecond median and min-max spread.

Steady-state throughput is separate: each process performs two unmeasured
warmup passes, then seven timed full-corpus extraction passes in process. The
harness reports its median extraction-only seconds; the runner reports the
median documents/sec across nine such invocations. This excludes executable
launch and teardown from throughput.

On Windows, the runner retains the process handle and reads the kernel process
memory counter through `GetProcessMemoryInfo` after exit. The reported value is
`PeakWorkingSetSize`, labelled peak working set rather than RSS, for both cold
and in-process runs.

## Coverage

| Format | Rust production extractor | Go prototype | Structured comparison |
| --- | --- | --- | --- |
| Markdown | `pulldown-cmark` plus byte spans | Markdown inline/autolink/reference parser | URL, text location, raw span |
| HTML | `html5tokenizer` plus byte spans | `x/net/html` tokenizer | URL, text location, raw span |
| Plain text | `linkify` plus byte spans | URL parser over tokenized prose | URL, text location, raw span |
| DOCX | `zip` plus `quick-xml` | `archive/zip` plus `encoding/xml` | URL and paragraph location |
| XLSX | `zip` plus `quick-xml` | `archive/zip` plus `encoding/xml` | URL and sheet/cell location |
| PPTX | `zip` plus `quick-xml` | `archive/zip` plus `encoding/xml` | URL and slide location |
| PDF | `lopdf` pages, annotations, and content streams | Minimal object/stream parser with FlateDecode and URI annotations | URL, page, and annotation location |

The Go PDF reader is deliberately minimal and covers the object, stream, and
annotation structures generated here, rather than the complete PDF grammar. The
Go port remains benchmark-only and is not a product implementation.
