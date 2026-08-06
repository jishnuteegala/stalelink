# Parse Benchmark Receipts

These are the language-decision receipts for brief decision 18: Rust remains
the implementation language for stalelink. This is a measurement record, not a
marketing claim. The comparison is intentionally limited to local document
parsing and link extraction; it does not include walking, checking, reporting,
or fixing.

## Results

| Implementation | Median docs/sec | Peak RSS (MiB) | Cold-start to first result (ms) |
| --- | ---: | ---: | ---: |
| Rust production extraction API | 205.34 | 7.60 | 78.06 |
| Go throwaway port | 233.56 | 7.43 | 65.56 |

The Go port won all three measurements on this machine and generated corpus.
That result does not reverse the language decision: the Rust side is the actual
production extraction implementation with rich locations and byte spans where
they are meaningful, while the Go comparison intentionally has less semantic
work. It is still a useful lower-bound receipt for the parsing alternative.

## Corpus

`bench/rust-harness` creates the corpus at benchmark time from a fixed
`0x5EED_C0DE` PRNG seed. No generated file or binary blob is committed.

| Size tier | Files per format | Links per file | Total files | Total links |
| --- | ---: | ---: | ---: | ---: |
| Small | 1 | 10 | 7 | 70 |
| Medium | 1 | 100 | 7 | 700 |
| Large | 1 | 500 | 7 | 3,500 |
| Total | 3 | - | 21 | 4,270 |

Each tier includes Markdown, HTML, UTF-8 plain text, DOCX, XLSX, PPTX, and
PDF. Text formats contain ordinary surrounding prose and HTTP URLs. OOXML files
contain external hyperlink relationships wired into document XML, worksheet
hyperlinks, and presentation hyperlink clicks. PDFs contain extracted-text URLs.
The generated run was 0.44 MiB on disk (small 0.01 MiB, medium 0.07 MiB, large
0.36 MiB).

## Method

Run `powershell.exe -NoProfile -File bench/run.ps1` from the repository root.
The runner regenerates the corpus, builds release binaries, performs one
unmeasured warmup, then runs nine fresh processes per implementation. It reports
the median docs/sec over those nine process runs. Cold-start is one separately
spawned process measured from `Start-Process` until it exits after emitting the
link count. This avoids calling a hidden product benchmark path: the Rust
harness is an excluded, `publish = false` crate that calls the public
`stalelink-core` extraction API.

On Windows, peak RSS is approximated by sampling each child process's
`WorkingSet64` every 10 ms until exit and retaining the greatest sample. This is
not an OS kernel peak-working-set counter and can under-report very short-lived
allocation spikes. The runner writes raw results and machine data under ignored
`bench/results/` so a receipt can be reproduced or refreshed.

Measured on 2026-08-06:

- OS: Windows 11 Home, build 26200 (10.0.26200)
- CPU: 11th Gen Intel Core i7-11800H, 8 cores / 16 logical processors
- Memory: 15.68 GiB physical RAM
- Toolchains: Rust stable/MSVC; Go 1.26.5 windows/amd64

## Coverage And Caveats

| Format | Rust implementation | Go port implementation | Comparison fidelity |
| --- | --- | --- | --- |
| Markdown | `pulldown-cmark`, byte spans | URL regex | Counts for generated inline URLs; no Markdown semantics or spans in Go |
| HTML | `html5tokenizer`, byte spans | `href` regex | Counts for generated quoted anchors; no malformed/entity handling or spans in Go |
| Plain text | `linkify`, byte spans | URL regex | Comparable for generated URLs; punctuation semantics differ |
| DOCX | `zip` + `quick-xml` | `archive/zip` + relationship regex | External relationship counts comparable; Go has no fields, paragraphs, or locations |
| XLSX | `zip` + `quick-xml` | `archive/zip` + relationship regex | External relationship counts comparable; Go has no formulas, cells, or locations |
| PPTX | `zip` + `quick-xml` | `archive/zip` + relationship regex | External relationship counts comparable; Go has no slide relationship use validation or locations |
| PDF | `lopdf` text extraction plus annotations | URL regex over generated uncompressed content stream | Generated bare-URL count comparable; Go has no PDF parser, annotations, pages, or locations |

Both programs output only the total link count. The runner verifies equal totals
(4,270 in this receipt) by visibly printing each implementation's count before
the measurements. Go is explicitly a throwaway prototype in `bench/go-port/`;
it has no Cargo workspace membership and no cargo-dist metadata. `cargo
metadata --no-deps --format-version 1` confirms the workspace contains only
`stalelink-core` and `stalelink`. `cargo dist plan` could not be run on this host
because `cargo-dist` is not installed; no release configuration was changed.
