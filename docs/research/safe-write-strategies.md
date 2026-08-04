# Safe Minimal-Diff Write Strategies for Auto-Fix

Date: 2026-08-04
Issue: https://github.com/jishnuteegala/stalelink/issues/7
Scope: RESEARCH ONLY - grounded in primary sources (docs.rs, crate repos, OOXML references). No application code.

## Question

What are the safe, minimal-diff WRITE strategies for stalelink's auto-fix stage, per format?
Three parts:
(a) OOXML: rewrite only the touched zip entries and copy the rest byte-for-byte, preserving order and compression so diffs stay minimal and the file stays valid.
(b) MD/HTML/plain text: byte-offset splicing vs parse-and-reserialize.
(c) Post-write verification: re-parse to confirm the new URL is present and the file still opens, and byte-restore the original on failure.

## Summary of settled context (from sibling ticket)

OOXML link extraction uses the `zip` crate + quick-xml 0.41 (MIT).
DOCX/XLSX/PPTX are zip containers; hyperlinks live in relationship parts (word/_rels/document.xml.rels etc.) and in HYPERLINK field codes.
Byte offsets in XML come from quick-xml buffer_position().
Markdown uses pulldown-cmark 0.13 with into_offset_iter() byte spans.
HTML uses lol_html 3.0 (BSD-3) with SourceLocation::bytes(); the rewriter can perform the fix itself.
Plain text uses linkify 0.11 with start()/end() byte indices.

## (a) OOXML write: rewrite touched entries, raw-copy the rest

### The zip crate supports read-and-selective-rewrite

Source: docs.rs zip 8.6.0 (MIT), https://docs.rs/zip/latest/zip/write/struct.ZipWriter.html and https://docs.rs/zip/latest/zip/read/struct.ZipArchive.html.
The `zip` crate is the right tool; a full manual rebuild from scratch is NOT required.

The key primitive is `ZipWriter::raw_copy_file`:
`pub fn raw_copy_file<R: Read>(&mut self, file: ZipFile<'_, R>) -> ZipResult<()>`.
Its doc: "Add a new file using the already compressed data from a ZIP file being read, this allows faster copies of the ZipFile since there is no need to decompress and compress it again. Any ZipFile metadata is copied and not checked, for example the file CRC."
This copies the entry's already-compressed bytes verbatim, so untouched entries keep their exact compressed representation - the minimal-diff requirement.

There is also `raw_copy_file_rename` (copy + rename) and `merge_archive` (bulk verbatim copy of an entire source archive via a single io::copy, faster than per-entry raw_copy_file).

### Recommended write loop (rebuild-in-order, raw-copy untouched)

The robust, order-preserving approach:
1. Open the source with `ZipArchive::new(reader)` (reads the central directory; `by_index` gives entries in stored order).
2. Create a fresh `ZipWriter::new(cursor_or_temp_file)`.
3. Iterate `for i in 0..archive.len()` in order:
   - Untouched entry: `archive.by_index_raw(i)` then `dst.raw_copy_file(file)` - byte-for-byte, no recompress, order preserved.
   - Touched entry (the .rels or sheet/slide XML with the changed URL): `dst.start_file(name, options)` then write the edited XML bytes. Match the original compression method (typically Deflated for OOXML) so the diff stays minimal.
4. `dst.finish()`.

Note on `new_append`: `ZipWriter::new_append(readwriter)` reopens an existing archive ready for append, but it cannot rewrite an entry in the middle without leaving the old copy; for a clean minimal-diff replace of one entry, the rebuild-in-order loop above is safer and preserves entry order deterministically.
Use `by_index_raw` (not `by_index`) for untouched entries so the compressed data is fed to `raw_copy_file` without a decompress/recompress round-trip.

### .rels Target attribute vs HYPERLINK field codes need different handling

These are two distinct link locations inside the OOXML package and the edit differs.

1. Relationship parts (external hyperlinks).
The touched entry is a `_rels/*.rels` part, e.g. `word/_rels/document.xml.rels`, `xl/worksheets/_rels/sheetN.xml.rels`, `ppt/slides/_rels/slideN.xml.rels`.
Each hyperlink is a `<Relationship>` element with `Type=".../hyperlink"`, `Target="URL"`, and usually `TargetMode="External"`.
The document body (document.xml / sheetN.xml / slideN.xml) references it only by `r:id` (the relationship Id), NOT by URL.
So the URL string lives ONLY in the .rels part's `Target` attribute.
The fix: edit the `Target` attribute value in that one .rels entry; the body XML entry is untouched and raw-copied.
This is the common, clean case - one attribute value in one small XML part.

2. HYPERLINK field codes (URL embedded in the document body).
In DOCX field-code hyperlinks the URL is stored as literal text inside the field instruction, e.g. a run containing `HYPERLINK "https://old.example"` inside `<w:instrText>` (or split across several `w:instrText`/`w:fldSimple` runs).
Here the URL is NOT in a .rels Target; it is character data in the body part (document.xml).
The fix: edit the `w:instrText` / `w:fldSimple w:instr` text in the body entry itself.
Caveat: field instruction text can be split across multiple runs, so the URL may span several `<w:instrText>` elements; the extractor must reassemble the instruction to locate the URL and then splice the correct byte range(s) in the body XML.

Practical rule for stalelink: determine which entry actually holds the URL string (a .rels Target vs a body field-code run), edit only that entry's XML bytes, and raw-copy everything else.
Do not rewrite the body part when only a .rels Target changed, and vice versa - keeping the diff to a single zip entry.

## (b) MD / HTML / plain text: byte-offset splicing, back-to-front

### Splicing is the right call; parse-and-reserialize is not

For Markdown, HTML, and plain text the safe minimal-diff write is byte-offset splicing:
replace the exact byte range of the old URL with the new URL bytes and leave every other byte untouched.

Parse-and-reserialize is the wrong call for auto-fix:
- pulldown-cmark is a parser/event stream; there is no round-tripping serializer that reproduces the author's exact source, so reserializing would reflow and normalize formatting (list markers, emphasis style, link reference vs inline style, whitespace, hard breaks).
- Re-emitting HTML from a DOM normalizes quoting, attribute order, self-closing style, and whitespace, destroying the author's formatting even where nothing was linked.
- Splicing touches only the changed URL bytes, so the resulting diff is exactly the URL change and nothing else.

The byte ranges to splice come directly from the extractors already chosen:
- Markdown: pulldown-cmark 0.13 `into_offset_iter()` yields a `Range<usize>` byte span for each event; the destination URL of a link/image event gives the exact old-URL byte range.
- HTML: lol_html 3.0 `SourceLocation::bytes()` gives the byte range; note lol_html is itself a streaming rewriter, so the fix can be done inside the rewriter (rewrite the `href`/`src` attribute value as the stream passes) rather than by external splicing - this is the preferred HTML path and inherently preserves surrounding bytes.
- Plain text: linkify 0.11 `Link::start()` / `Link::end()` give the byte range of the matched URL.

### Ordering: apply edits back-to-front

When multiple URLs in one file are fixed, applying an edit shifts the byte offsets of everything after it whenever the replacement length differs from the original.
To keep every remaining offset valid, sort the edits by start offset and apply them in descending order (highest offset first, i.e. back-to-front).
Because each applied edit is entirely after the not-yet-applied ones, earlier offsets are never invalidated.
Equivalent alternative: build the output by walking edits front-to-back while copying the gaps between them into a new buffer; back-to-front in-place splicing is simplest and is the recommended approach.
This ordering concern does not apply to the lol_html streaming path (it rewrites in a single forward pass) but does apply to Markdown and plain text splicing.

## (c) Post-write verification and byte-restore rollback

Overarching rule: stalelink keeps the original bytes in memory (or a sibling temp copy) before writing.
After writing, it re-opens and re-parses the new file to confirm two things: the file still opens/parses, and the new URL is present where the old one was.
If either check fails, it restores the original bytes over the written file (a plain byte-for-byte overwrite), leaving the document exactly as found.

Write atomically where possible: write to a temp file in the same directory, verify it, then rename over the original.
Rename-over-original is atomic on the same filesystem and means a crash mid-write never leaves a half-written document; the original stays intact until the verified temp replaces it.
Keeping the original bytes additionally covers the case where verification of the new file fails after the rename.

Per-format verification:
- OOXML: re-open the output with `ZipArchive::new`; confirm it parses (valid central directory), read back the touched entry (the .rels or body part) and re-parse its XML with quick-xml to confirm the new URL string is present and the old one is gone. Optionally confirm `archive.len()` and entry names are unchanged.
- Markdown: re-parse the spliced bytes with pulldown-cmark and confirm a link/image event now carries the new destination URL at the expected location; parsing succeeding also confirms the file is still well-formed Markdown text.
- HTML: re-parse with lol_html (or a second rewriter pass) and confirm the target element's `href`/`src` now equals the new URL; a clean pass confirms the document still parses.
- Plain text: re-scan with linkify and confirm the new URL is found at the spliced range and the old URL is absent. Text has no structural validity to break, so the presence check is the verification.

Rollback: on any verification failure, overwrite the file with the retained original bytes (or discard the temp file if the atomic-rename has not happened yet). Report the fix as failed for that file so the user knows it was left unchanged.

## Per-format write mechanism table

| Format | Where the URL lives | Write mechanism | Crate(s) | Verification | Rollback |
| --- | --- | --- | --- | --- | --- |
| DOCX/XLSX/PPTX (.rels) | `<Relationship Target="URL">` in `_rels/*.rels` | Rebuild zip in order: `raw_copy_file` untouched entries, `start_file` + edited XML for the touched .rels part | zip 8.6.0 (MIT), quick-xml 0.41 | Re-open with `ZipArchive::new`; re-parse touched .rels; new URL present, old gone | Overwrite with retained original bytes |
| DOCX HYPERLINK field code | URL text in `w:instrText`/`w:fldSimple` in body part | Same zip rebuild, but edit the body part's field-instruction bytes (may span runs) | zip 8.6.0, quick-xml 0.41 | Re-open archive; re-parse body part; reassembled instruction has new URL | Overwrite with retained original bytes |
| Markdown | Link/image destination | Byte-offset splice, edits applied back-to-front | pulldown-cmark 0.13 (`into_offset_iter`) | Re-parse; link event carries new URL | Overwrite with retained original bytes |
| HTML | `href` / `src` attribute | In-place rewrite during lol_html streaming pass (no external splice needed) | lol_html 3.0 (BSD-3, `SourceLocation::bytes`) | Re-parse; attribute equals new URL | Overwrite with retained original bytes |
| Plain text | Matched URL substring | Byte-offset splice, edits applied back-to-front | linkify 0.11 (`start`/`end`) | Re-scan; new URL at spliced range, old absent | Overwrite with retained original bytes |

## Primary sources

- zip 8.6.0 ZipWriter (raw_copy_file, raw_copy_file_rename, merge_archive, new_append, start_file, finish): https://docs.rs/zip/latest/zip/write/struct.ZipWriter.html
- zip 8.6.0 ZipArchive (new, by_index, by_index_raw, len, file_names): https://docs.rs/zip/latest/zip/read/struct.ZipArchive.html
- zip crate repository: https://github.com/zip-rs/zip2
- OOXML relationship parts and hyperlink relationships: ECMA-376 Open Packaging Conventions (relationship Target/TargetMode, r:id references).
