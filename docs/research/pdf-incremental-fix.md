# PDF Incremental-Update Auto-Fix Feasibility

Date: 2026-08-04
Issue: https://github.com/jishnuteegala/stalelink/issues/6
Scope: RESEARCH ONLY. Grounded in lopdf 0.44 source (docs.rs) and PDF 32000-1 references.

## Question

How feasible is rewriting a link-annotation URI via PDF INCREMENTAL UPDATE in Rust using lopdf's write path?
Cover: (a) the annotation-URI splice + incremental-append write mechanics and whether lopdf supports incremental save;
(b) refusing encrypted and digitally-signed documents;
(c) re-parse verification;
(d) bare text URLs that are not annotations.
Deliver a go/no-go per fix case plus a refusal-detection checklist.

## Context (settled by sibling ticket)

PDF reading uses lopdf 0.44 (MIT).
It gives a writable in-memory object model.
Link locations are page + annotation /Rect; the /URI is directly splice-able in the object model.

## lopdf write capabilities: incremental vs full

lopdf 0.44 supports BOTH a full re-save and a true incremental update.
The two write paths are distinct types.

### Full re-save (Document)

`Document::save`, `save_to`, `save_with_options`, `save_modern` all rewrite the ENTIRE file from the in-memory object model.
Source: src/lopdf/writer.rs, `save_internal` writes `%PDF-`, all objects, a fresh xref, then a fresh trailer.
This produces a clean single-revision PDF but discards the original byte layout of every object.
There is no `/Prev` chain; the previous revision is gone.

### True incremental update (IncrementalDocument)

lopdf 0.44 ships a dedicated `IncrementalDocument` type that does exactly what the PDF spec calls an incremental update.
Source: src/lopdf/incremental_document.rs and the `impl IncrementalDocument` block in src/lopdf/writer.rs (lines 264-414).

Load: `IncrementalDocument::load(path)` / `load_from(reader)` / `load_mem(bytes)`.
It keeps the ORIGINAL file bytes (`get_prev_documents_bytes`) plus a parsed view of the prior revisions (`get_prev_documents` -> `&Document`).
Edits go into a separate `new_document: Document` field.

Edit workflow:
1. `opt_clone_object_to_new_document(object_id)` copies an existing object from the prior revision into `new_document` so it can be mutated (no-op if already present).
2. Mutate `new_document` objects via the normal `Document` API (`get_object_mut`, `get_dictionary_mut`, etc.).
3. `save`/`save_to` appends.

Save mechanics (writer.rs `IncrementalDocument::save_internal`):
- Writes the previous document bytes verbatim first.
- Appends a newline if the prior bytes did not end in one.
- Writes `%PDF-<version>` and a binary mark, then each NEW indirect object (appended body objects).
- Writes a NEW xref section for only the appended objects.
- Writes a NEW trailer whose `/Prev` points at the prior `xref_start` (set in `Document::new_from_prev`, trailer key `Prev`).
- Ends with `startxref <new-xref-offset>` and `%%EOF`.

Conclusion: we do NOT need to hand-roll the incremental append.
lopdf provides it natively and correctly (appended objects + new xref + new trailer with /Prev).
This is the recommended path because it preserves the original bytes, which matters for provenance and for not disturbing anything we did not touch.

## Annotation-URI fix mechanism

A Link annotation is a page annotation dictionary with `/Subtype /Link`.
Its action lives in `/A`, an action dictionary with `/S /URI` and `/URI (the-url)` (PDF 32000-1, 12.5.6.5, 12.6.4.7).
The `/URI` value is a PDF string object, directly editable in lopdf's object model.

Locating the annotation:
- `Document::get_pages()` gives page number -> page ObjectId.
- `Document::get_page_annotations(page_id)` returns the annotation dictionaries; filter `/Subtype == Link`.
- The sibling ticket already establishes we know the page + annotation and that /URI is splice-able.

The action object may be inline (`/A << ... >>` directly in the annotation) or an indirect reference (`/A 12 0 R`).
Both cases must be handled:
- Inline: the /A dict is part of the annotation object, so the object we clone-and-edit is the annotation object itself.
- Indirect: the /A action is its own indirect object; that object is the one to clone-and-edit.

Incremental-update splice (recommended path):
1. `IncrementalDocument::load(path)`.
2. Resolve the object id that actually holds the `/URI` string (the annotation object for inline /A, or the referenced action object for indirect /A).
3. `opt_clone_object_to_new_document(that_id)` to bring it into `new_document`.
4. `new_document.get_dictionary_mut(that_id)` then navigate to the /A action dict and `set(b"URI", Object::string_literal(new_url))`.
   For inline /A, edit the nested dict in place; for indirect /A, edit the cloned action object.
5. `save`/`save_to` appends the changed object(s) + new xref + trailer with /Prev.

Because only the object(s) carrying the URI are re-emitted, every other byte of the file is preserved verbatim.
This is the smallest possible change and the safest for provenance.

GO for annotation-URI fix via incremental update.

## Refusal detection: encrypted and signed documents

We MUST refuse to fix two classes of document, because a write would either fail or silently corrupt/invalidate them.

### Encrypted (/Encrypt in trailer)

Detection (lopdf, from src/lopdf/document.rs):
- `Document::is_encrypted()` returns true when the trailer has a resolvable `/Encrypt` entry.
- `Document::get_encrypted()` returns the encryption dictionary itself.
- `Document::was_encrypted()` returns true if the doc was decrypted after load (encryption_state is set).

lopdf enforces this on the incremental path too.
`IncrementalDocument::save` calls `check_incremental_save_supported` BEFORE `File::create`, so an unsupported (still-encrypted) input does not truncate an existing output file.
That guard returns `ErrorKind::Unsupported` with the message
"incremental update of a still-encrypted PDF is not supported: call Document::decrypt on the previous revision first"
(see lopdf issue 520).

Policy for stalelink v1: if `is_encrypted()` is true, REFUSE the fix outright.
We do not attempt to decrypt (even the common empty-password case) because re-writing an encrypted doc is out of scope and risky.
Report the file as "found dead links but cannot auto-fix: encrypted".

### Digitally signed (signature would break)

An incremental update is actually the signature-preserving way to add content, BUT any change to bytes covered by an existing signature's ByteRange invalidates that signature, and editing a URI in a signed region does exactly that.
For v1 we refuse rather than risk emitting a file whose signature now fails validation.

Detection signals (all via the object model; PDF 32000-1, 12.7.4.5 and 12.8):
- AcroForm with signature fields: catalog `/AcroForm` -> `/Fields`; any field with `/FT /Sig` (and typically a populated `/V` signature dictionary) means the document is signed.
- `/Perms` in the catalog: presence of `/Perms` (e.g. `/DocMDP`, `/UR3`) indicates a certification/usage-rights signature that constrains modification.
- A signature dictionary has `/Type /Sig` with a `/ByteRange` and `/Contents`; its presence anywhere confirms a real signature (not just an empty sig field).

Access path in lopdf: `Document::catalog()` -> get `b"AcroForm"` (deref) -> get `b"Fields"` array -> for each field dict, check `/FT == Sig` and whether `/V` resolves to a dict with `/ByteRange`.
Also check catalog for `b"Perms"`.

Policy for stalelink v1: if any signature field is present OR `/Perms` is present, REFUSE the fix.
Report "found dead links but cannot auto-fix: digitally signed".

### Refusal-detection checklist (run before any write)

1. `is_encrypted()` true                              -> REFUSE (encrypted).
2. catalog `/Perms` present                            -> REFUSE (certified/usage-rights signature).
3. catalog `/AcroForm` -> `/Fields` has a `/FT /Sig`   -> REFUSE (signed).
   field whose `/V` resolves to a sig dict with
   `/ByteRange` and `/Contents`
4. Otherwise                                           -> eligible for incremental-update fix.

## Re-parse verification

After writing, reopen the output and confirm the change landed and the document still parses.
This is cheap and catches a broken append (e.g. bad xref offset) before we replace the user's file.

Verification steps:
1. Reload the written file with `Document::load(path)` (or `IncrementalDocument::load` then inspect the combined view).
   A successful load already proves the xref chain and trailer parse, because lopdf follows `/Prev` to reconstruct the object model.
2. Re-resolve the same page + annotation and read `/A -> /URI`.
   Assert it equals the new URL (and no longer equals the old one).
   Because the newest revision's object shadows the prior one (last /Prev wins), `get_object` returns the appended, edited version.
3. Sanity-check that `get_pages()` still returns the same page count and that `catalog()` resolves.
   This guards against a trailer that parsed but lost `/Root`.

Note on lopdf's own model: a loaded `Document` "can be a combination of multiple incremental updates or just one (the last) incremental update" (document.rs doc comment).
So reloading a file we incrementally updated yields the merged view with our edit on top, which is exactly what verification needs.

Write to a temp path first, verify, then atomically replace the original.
If verification fails, discard the temp file and report the fix as failed (leave the original untouched).

## Text-URL fix go/no-go (bare URLs that are NOT annotations)

A bare URL printed as page text is not an annotation.
It is a sequence of glyph-showing operators (`Tj`, `TJ`, `'`, `"`) inside a page content stream.
lopdf can rewrite content-stream text: `Document::replace_text` and `Document::replace_partial_text` exist and edit the decoded content stream, and `change_page_content` / `change_content_stream` let you swap a whole stream.

However, content-stream text replacement is NOT viable as a reliable auto-fix for URLs in v1, for several reasons:

1. Text is frequently NOT stored as contiguous ASCII.
   A URL can be split across many `Tj`/`TJ` operators, kerned character by character, so "http://old" may never appear as one findable substring.
2. Glyphs are indexed through the font's encoding.
   With subset/custom-encoded or CID fonts, the bytes in the stream are glyph ids, not the URL characters; a naive string search finds nothing or the wrong bytes.
3. Replacing a shorter/longer URL shifts glyph positions and can break layout, and `replace_text` operates per-page on decoded content with its own matching caveats (README notes exact vs partial matching).
4. A bare text URL has no clickable target to "fix" - only the visible string. Changing visible text silently is a different and riskier operation than repointing a link's target.

Therefore: SCOPE TEXT-URL FIXES OUT for PDF in v1.
stalelink should still DETECT and REPORT dead bare-text URLs (reading is fine), but not auto-rewrite them.
Revisit later only with font-encoding-aware matching and layout-preservation, if there is demand.

NO-GO for text-URL auto-fix in v1 (detect/report only).

## Final go/no-go summary

| Fix case            | Verdict        | Mechanism |
| ------------------- | -------------- | --------- |
| Annotation URI      | GO             | `IncrementalDocument::load` -> `opt_clone_object_to_new_document` on the object holding `/URI` -> set `/A /URI` to new string -> `save` (appends objects + new xref + trailer with /Prev). |
| Text URL (bare)     | NO-GO (v1)     | Detect/report only. Content-stream text replacement is unreliable (split runs, font encoding, layout). |
| Encrypted doc       | REFUSE         | `is_encrypted()` true; also enforced by lopdf's `check_incremental_save_supported`. |
| Signed doc          | REFUSE         | Catalog `/Perms`, or AcroForm sig field (`/FT /Sig` with `/V` -> `/ByteRange`); editing invalidates the signature. |

### Key findings

- lopdf 0.44 supports TRUE incremental update natively via `IncrementalDocument`; no hand-rolled append needed.
- The incremental save preserves all original bytes and appends only changed objects, a new xref section, and a new trailer with `/Prev`.
- A full re-save (`Document::save`) also works but rewrites the whole file and drops the original byte layout; prefer incremental for auto-fix.
- Encryption refusal is both a policy check (`is_encrypted()`) and enforced by lopdf before it truncates any output.
- Signature refusal must be an explicit stalelink check (catalog `/Perms` and AcroForm signature fields); lopdf does not block this for us.
- Always write to a temp file, re-parse to verify the URI changed and the doc still loads, then atomically replace.
