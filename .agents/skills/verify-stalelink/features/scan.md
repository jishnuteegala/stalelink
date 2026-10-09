# Scan documents

Scan walks files or directories for supported document formats, extracts links,
checks local targets (files, anchors, mailto/tel) and HTTP endpoints, and
prints a findings report on stdout or writes it to a file, with the exit code
signalling whether findings met the `--fail-on` threshold.

## Sub-features

- `scan-table` prints a human-readable findings table on stdout.
- `scan-json` emits the stable machine envelope (`schema_version: 1`) on
  stdout or via `-o`.
- `scan-sarif` emits SARIF for code-scanning upload via `-o file.sarif`.
- `scan-local` validates file-path, anchor, mailto, tel, and relative links
  without network access.
- `scan-http` probes HTTP/HTTPS URLs and reports dead endpoints.
- `scan-exit` returns 0 when clean and 1 when findings meet the fail
  threshold.

## How to get to it (user POV)

- Run `stalelink scan <path>` in a terminal.
- Run `stalelink scan --format json <path>` or `stalelink scan --format sarif
  -o report.sarif <path>` for machine or code-scanning consumption.
- Invoke it in CI where the exit code gates the job.

## Driving it with the shell

Preconditions:

- Doctor passed; `RUN_DIR` with `scratch/` and `evidence/` exists;
  `STALELINK_CACHE_DIR` points at `scratch/cache`.
- Scratch HTTP server running per `Helpers` when `scan-http` is under test;
  its port is in `scratch/port.txt`.

- **Local findings.** Create `scratch/docs/guide.md` containing a link to a
  real sibling file, a link to `missing.md`, and an anchor link to a missing
  heading. Run `stalelink scan --no-cache scratch/docs`. stdout lists the
  missing-file and missing-anchor findings; exit code is 1.
- **HTTP liveness.** Add `http://127.0.0.1:<port>/ok` and
  `http://127.0.0.1:<port>/dead` to the fixture and rescan with `--no-cache`.
  `/dead` produces a dead-link finding while `/ok` stays clean; exit code
  stays 1.
- **JSON envelope.** Run `stalelink scan --no-cache --format json
  scratch/docs > evidence/scan.json`. The file parses as JSON with the
  document and findings shape from `schema/stalelink-report.v1.json`.
- **SARIF artifact.** Run `stalelink scan --no-cache --format sarif -o
  scratch/out.sarif scratch/docs`. `out.sarif` exists on disk and is valid
  SARIF (`version: "2.1.0"`).
- **Fail threshold.** Run with `--fail-on dead-certain` on a fixture whose
  only finding is `suspect`-level; exit code is 0 despite findings.

## Gotchas

- Without `--no-cache`, a second identical run can mask a real check through
  cached verdicts; always isolate or bypass the cache.
- Glob and exclude flags (`--include`, `--exclude`, `--exclude-url`,
  `--exclude-domain`) shrink the corpus silently; an unexpectedly empty report
  usually means an over-broad exclusion rather than clean documents.
- `--auth` tiers above `off` touch real browser state; verification drives
  stay on `--auth off` (the default) and treat browser tiers as untested.
- stdout carries findings while stderr carries progress/diagnostics; capture
  both or a `-q` run can look like it produced nothing.
