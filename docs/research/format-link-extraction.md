# Research: per-format link extraction crates

- Date: 2026-08-04
- Question: For each v1 format (PDF, DOCX/XLSX/PPTX, Markdown, HTML, plain text), which Rust crate(s) should extract hyperlinks, what counts as a link, what location info is available for a later auto-fix splice stage, plus license and maintenance health.
- Issue: https://github.com/jishnuteegala/stalelink/issues/2
- Sources: docs.rs API docs, crates.io metadata, crate repo READMEs.

## PDF

What counts as a link in a PDF:

- Link annotations: `/Annot` dictionaries with `/Subtype /Link` carrying a `/A` action (commonly `/URI`) or a `/Dest` destination, positioned by a `/Rect` on a page.
- Bare text URLs: URL-looking strings inside content-stream text, with no annotation.
Extracting these requires text extraction plus a plain-text URL scanner (see the plain-text section).

| Crate | Version | License | Link sources covered | Location info | Health |
| --- | --- | --- | --- | --- | --- |
| lopdf | 0.44.0 | MIT | Full object-model access: walk page `/Annots`, read `/Subtype /Link`, `/A` `/URI`, `/Dest`; can also decode content streams for text | Object IDs, page object references, annotation `/Rect` coordinates; because lopdf is a read/write object model, the same object can be mutated and the document saved (native splice path) | Active; 0.44.0 released 2026-07-10; pure Rust, Rust 1.85+, no native deps |
| pdfium-render | 0.9.3 | MIT OR Apache-2.0 | `PdfPageLinks` collection per page; `PdfLink::action()` -> `PdfAction` (URI and other action types), `PdfLink::destination()`, `PdfPageLinkAnnotation`; strong text extraction for bare-URL scanning | Page index, `PdfLink::rect()` bounding box in page points; no byte offsets into the raw file | Active; 0.9.3 released 2026-07-14; but binds at run time to a native Pdfium (BSD-3-Clause) shared library that must be shipped or found on the system |

Notes:

- lopdf gives direct byte-level and object-level access, which matters for auto-fix: a `/URI` string can be replaced in the object model and the file re-serialized without touching layout.
- pdfium-render has the nicer high-level link API but adds a native dependency, which hurts a portable single-binary CLI; its output locations are visual (page + rect), not spliceable offsets.
- Recommendation for stalelink: lopdf for annotation links and document rewrite; pair with text extraction (lopdf content-stream decoding or a helper crate) plus linkify for bare text URLs.

## DOCX / XLSX / PPTX (OOXML)

What counts as a link in OOXML (all three are ZIP containers of XML parts):

- Relationship parts: `word/_rels/document.xml.rels`, `xl/_rels/workbook.xml.rels` plus per-sheet `.rels`, `ppt/slides/_rels/slideN.xml.rels`; external hyperlinks are `<Relationship Type=".../hyperlink" Target="https://..." TargetMode="External"/>`, referenced from the body by `r:id` on `<w:hyperlink>`, `<hyperlink>` (spreadsheet), or `<a:hlinkClick>`.
- HYPERLINK field codes in DOCX: `<w:fldSimple w:instr="HYPERLINK ...">` and split `<w:instrText>HYPERLINK "url"</w:instrText>` runs; these bypass the rels part.
- XLSX HYPERLINK() worksheet formulas inside `<f>` elements, and cell `<hyperlink ref="A1" r:id="..."/>` entries in the sheet XML.
- Bare text URLs in run text (`<w:t>`, shared strings, `<a:t>`), found by plain-text scanning.

| Crate | Version | License | Link sources covered | Location info | Health |
| --- | --- | --- | --- | --- | --- |
| quick-xml | 0.41.0 | MIT | All of the above, since we parse the XML parts ourselves: rels entries, `w:hyperlink`/`hlinkClick` `r:id`, field codes, formulas | `Reader::buffer_position()` gives the byte offset into each XML part after every event; combined with the part name and surrounding context (paragraph index, sheet name + `ref` cell attribute, slide number from part path) this fully supports splicing | Very active; 0.41.0 released 2026-06-29; de-facto standard Rust XML parser |
| zip | 8.6.0 | MIT | Container access: enumerate and read parts, rewrite the archive with modified parts | Part (file-within-zip) granularity | Active; maintained under the zip-rs org |
| docx-rs | 0.4.22 | MIT | DOCX only; primarily a writer with a reader that models hyperlinks | Structured model (paragraph/run), but no byte offsets into the original XML; lossy round-trip risk for auto-fix | Active (0.4.22, 2026-07-21) but scope is DOCX-only and writer-focused |
| calamine | 0.36.1 | MIT | XLSX read only (values and formulas); no write, hyperlink relationship support is limited | Cell references (sheet, row, col) | Very active, but read-only so it cannot serve the auto-fix stage |

Notes:

- No single high-level crate covers DOCX+XLSX+PPTX hyperlinks with fix-capable locations; the honest v1 answer is `zip` + `quick-xml` with a small OOXML layer owned by stalelink.
- Splice model: record (part path, byte range of the attribute value or text node) via `buffer_position()` deltas, then rewrite that part and repack the zip; this preserves everything else in the document byte-for-byte.
- Human-readable location for reporting: DOCX paragraph ordinal, XLSX sheet + cell ref (from the `<hyperlink ref>` or `<c r>` attribute), PPTX slide number from the part name.

## Markdown

What counts as a link in Markdown (CommonMark + common extensions):

- Inline links `[text](url "title")` and inline images `![alt](url)`.
- Reference links `[text][label]`, collapsed `[text][]`, shortcut `[text]`, resolved against link reference definitions `[label]: url`.
- Autolinks `<https://example.com>` and, with the GFM extension, bare-URL autolinks in plain text.
- Raw HTML blocks/inline HTML inside the Markdown (delegate to the HTML path).

| Crate | Version | License | Link sources covered | Location info | Health |
| --- | --- | --- | --- | --- | --- |
| pulldown-cmark | 0.13.4 | MIT | `Tag::Link`/`Tag::Image` with `LinkType` distinguishing Inline, Reference, Collapsed, Shortcut, Autolink, Email, WikiLink; `Parser::reference_definitions()` exposes the definition map; GFM-style extensions via `Options` | `Parser::into_offset_iter()` yields `(Event, Range<usize>)` byte ranges into the source for every event, so the exact source span of each link is known; `reference_definitions()` entries also carry spans | Very active; 0.13.4 released 2026-05-20; the standard Rust CommonMark parser |

Notes:

- The offset iterator gives the span of the whole link event; to splice only the URL, narrow the range with a small scan inside the span (find the `](` ... `)` or the definition's URL token).
- For reference links, the URL lives at the definition site, not the usage site; `reference_definitions()` provides the definition span so the fix is applied once.

## HTML

What counts as a link in HTML:

- `href` on `<a>`, `<area>`, `<link>`, `<base>`.
- `src` on `<img>`, `<script>`, `<iframe>`, `<audio>`, `<video>`, `<source>`, `<embed>`, `<track>`.
- `srcset` on `<img>`/`<source>` (comma-separated URL + descriptor list; needs its own micro-parser).
- Others worth v1 consideration: `<form action>`, `<object data>`, `poster` on `<video>`, `cite`, and `meta http-equiv=refresh` content URLs.

| Crate | Version | License | Link sources covered | Location info | Health |
| --- | --- | --- | --- | --- | --- |
| lol_html | 3.0.1 | BSD-3-Clause | CSS-selector handlers over a streaming rewriter; any attribute on matched elements; built to rewrite attribute values in place (Cloudflare Workers heritage) | `Element::source_location()` -> `SourceLocation::bytes()` returns the absolute byte `Range<usize>` in the original input (documented: byte positions, no line numbers) | Very active; 3.0.1 released 2026-07-29; maintained by Cloudflare |
| scraper | 0.27.0 | ISC | Full DOM (html5ever) with CSS selector queries; easy read-only extraction of any attribute | No source byte offsets; html5ever's tree builder discards input positions, so locations cannot feed a splicer | Active; 0.27.0 released 2026-05-11 |

Notes:

- lol_html wins for stalelink on both counts: it exposes byte source locations for scanning, and its rewriter can itself perform the auto-fix (set the attribute and stream out the document) without a hand-rolled splice.
- BSD-3-Clause is MIT-compatible (permissive, attribution-only).
- scraper remains a fine choice for read-only analysis but is a dead end for the fix stage.

## Plain text (and bare-URL fallback for every format)

What counts as a link in plain text:

- Bare URLs with a scheme (`https://...`, `http://...`).
- Scheme-less `www.` links and email addresses, optionally.
- Boundary handling is the hard part: trailing punctuation, wrapping parens/brackets/quotes, Unicode and IDN hosts.

| Crate | Version | License | Link sources covered | Location info | Health |
| --- | --- | --- | --- | --- | --- |
| linkify | 0.11.0 | MIT OR Apache-2.0 | Bare URLs and emails with careful boundary trimming (trailing dot/comma stripped, balanced parens kept, `<...>` wrappers excluded); linear-time scan; used by lychee | `Link::start()` and `Link::end()` are byte indices into the input string, exactly what a splicer needs | Active; 0.11.0 released 2026-04-12; small, focused, well-tested |
| regex | 1.13.1 | MIT OR Apache-2.0 | A hand-written URL pattern; SIMD-accelerated literal prefiltering makes `https?://` scans very fast | `Match::start()`/`end()` byte offsets | Extremely healthy (rust-lang org) |

Notes:

- linkify beats a raw regex on correctness (trailing punctuation, parens, Unicode) at comparable speed, and it is exactly the fallback lychee uses.
- If a regex prefilter is ever needed for huge files, run `regex` to find `http` candidates and hand the surrounding window to linkify for boundary resolution; in practice linkify alone is linear and sufficient.
- Line/col for reporting is derived from the byte offset by counting newlines up to `start()`, a cheap one-pass computation stalelink can share across all offset-based formats.

## Recommendation

Recommended v1 crate stack:

| Format | Crate(s) | Why |
| --- | --- | --- |
| PDF | lopdf 0.44 (+ linkify on extracted text) | Pure Rust, MIT, read AND write object model; link annotations via `/Annots` walk with page + `/Rect` locations; mutate `/URI` objects directly for auto-fix; no native library to ship |
| DOCX/XLSX/PPTX | zip 8.x + quick-xml 0.41 | Only path that covers rels parts, HYPERLINK field codes, and sheet hyperlinks across all three formats; `buffer_position()` gives byte offsets per part for splice-and-repack; both MIT |
| Markdown | pulldown-cmark 0.13 | `into_offset_iter()` byte spans per link event, `LinkType` classifies inline/reference/autolink, `reference_definitions()` locates definition-site URLs; MIT |
| HTML | lol_html 3.0 | `SourceLocation::bytes()` byte ranges for scanning, and the streaming rewriter performs the fix itself; BSD-3-Clause (MIT-compatible); Cloudflare-maintained |
| Plain text / bare-URL fallback | linkify 0.11 (regex 1.13 optional prefilter) | Byte-offset `start()`/`end()`, correct boundary trimming, linear time; MIT OR Apache-2.0 |

Rationale summary:

- Every recommended crate reports byte offsets (or, for PDF, a writable object model), so the auto-fix stage has a uniform splice contract: (file or part, byte range, replacement).
- pdfium-render was rejected for v1: native Pdfium dependency conflicts with a portable single-binary CLI, and its locations are visual (page + rect) rather than spliceable.
- scraper was rejected for the fix stage: html5ever discards source positions.
- All licenses are MIT, MIT OR Apache-2.0, ISC, or BSD-3-Clause, all MIT-compatible.
- All crates shipped releases within the last 4 months as of 2026-08-04.
