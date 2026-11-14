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
  evidence/out.sarif scratch/docs` (not `scratch/`, which Cleanup deletes).
  `out.sarif` exists on disk and is valid SARIF (`version: "2.1.0"`).
- **Fail threshold.** Run with `--fail-on dead-certain` on a fixture whose
  only finding is below `dead-certain` - e.g. a redirect link producing an
  `OUTDATED` `PermanentRedirect` row; exit code is 0 despite findings.

## Gotchas

- Without `--no-cache`, a second identical run can mask a real check through
  cached verdicts; always isolate or bypass the cache.
- Glob and exclude flags (`--include`, `--exclude`, `--exclude-url`,
  `--exclude-domain`) shrink the corpus silently; an unexpectedly empty report
  usually means an over-broad exclusion rather than clean documents.
- `--auth` tiers above `off` touch real browser state, and the configured
  default is `cookies` - drives that must stay offline must pass `--auth off`
  explicitly and treat browser tiers as untested.
- The fail-on threshold compares by rank, not declaration order:
  `dead-certain > likely-dead > outdated > auth-walled > suspect`, so
  `--fail-on outdated` still passes auth-walled findings (rank 2 < 3) -
  the enum lists `AuthWalled` before `Outdated`, which is the trap.
- stdout carries findings while stderr carries progress/diagnostics; `-q`
  suppresses progress but notices/warnings/errors still print to stderr -
  the nothing-on-either-stream case is `-q` + `-o` on a diagnostic-free
  run. A fully clean table-format scan prints nothing on stdout even
  without `-q`/`-o` (json and sarif still emit their documents).
- The table's REASON column shows the Debug variant (`LocalMissing`,
  `PermanentRedirect`) except `HttpStatus`, which renders `HTTP <status>`;
  the human-readable detail (`anchor not found: #x`) is JSON-only - assert
  on URL + variant name or HTTP status, not the prose.
- The directory walk honors `.gitignore`, `.ignore` and global git excludes;
  an unexpectedly empty corpus inside a repository usually means ignore
  rules, not clean documents.
- `stalelink.toml` discovery for `scan`/`fix` walks up from the first scan
  path (an ancestor toml under TMPDIR can inject `output.fail-on` or
  `cache.dir`), while `cache stats`/`cache clear` resolve config from cwd;
  drives that must stay config-free should point `STALELINK_CACHE_DIR` and
  check for ancestor tomls.
- `--auth browser` on the default build (no `live-browser` feature) prints
  a warning and exits 3; `--min-confidence` filters display only and never
  gates the exit code.
