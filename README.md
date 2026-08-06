# stalelink

Scan documents for dead and outdated links.

`stalelink` walks your PDFs, Word/Excel/PowerPoint files, Markdown, HTML, and plain text, extracts every link, and tells you which ones are dead, redirected, login-walled, or stale - including links behind authentication, checked with your own browser's logged-in state so they are never falsely reported dead.

Fully local and open source: no telemetry. Network requests are made only for requested link checks; their verdicts are cached locally unless you use `--no-cache`.

> Status: pre-release - under active development. The feature set below describes what is being built; the first tagged release ships the complete local feature set.

## Planned surface

- **Formats:** PDF, DOCX, XLSX, PPTX, Markdown, HTML, plain text
- **Checks:** HTTP status, soft-404s, redirect chains, login walls, staleness signals (deprecated banners, versioned-URL drift), local file paths, intra-document anchors, cross-document relative links
- **Auth tiers:** plain HTTP -> browser-profile cookies -> real browser (CDP) for suspect links (Chrome, Edge, Brave, Chromium, Firefox)
- **Output:** human table, `--json`, SARIF; confidence levels (dead-certain, likely-dead, auth-walled, outdated, suspect); suggested replacement URLs; CI-friendly exit codes
- **Auto-fix:** `stalelink fix` previews link rewrites as a diff; `--write` applies them, across every supported format
- **Performance:** parallel parsing, URL dedupe, response caching, polite per-host rate limits

## Local links

Explicit Markdown destinations (including resolved reference links), HTML `href`/`src` attributes, and external PDF/OOXML targets may point to local files. Relative paths resolve from the containing document; absolute/root-relative paths resolve from the filesystem root. Query strings are ignored for filesystem lookup. Percent escapes in paths and fragments are decoded strictly before any filesystem lookup or anchor comparison; malformed escapes or invalid UTF-8 are syntax-invalid. Markdown headings use rendered text, lowercase slugs that retain Unicode letters and numbers plus hyphens and underscores, remove other characters, convert each whitespace character to a hyphen, and reserve GitHub-style document-wide duplicate suffixes (`-1`, `-2`). Markdown and HTML also recognize `id` on any element and `name` on `<a>` elements. Existing directories are valid fragmentless targets; links with directory fragments report that their anchors cannot be checked. Anchors in other existing formats are not inspected.

`mailto:` requires a simple `local@domain.tld` shape. `tel:` requires at least one digit and permits only digits, `+`, `-`, parentheses, and spaces. Use `--no-local` or `[ignore] local-links = true` to skip all local, mailto, and tel checks.

## Configuration and cache

`stalelink` discovers the nearest `stalelink.toml` by walking upward from the first scan input. Positional paths come first; paths read through `--stdin` are appended, so an stdin-only scan uses its first non-empty line. Multi-path scans deliberately use the first path's configuration. `cache clear` and `cache stats` discover configuration upward from the current directory.

Settings use `flags > environment > stalelink.toml > defaults` precedence. Environment names map explicitly to every supported setting: `STALELINK_NETWORK_MAX_CONCURRENCY`, `STALELINK_NETWORK_PER_HOST`, `STALELINK_NETWORK_TIMEOUT`, `STALELINK_NETWORK_RETRIES`, `STALELINK_NETWORK_USER_AGENT`, `STALELINK_CACHE_TTL`, `STALELINK_CACHE_DIR`, `STALELINK_AUTH_AUTH`, `STALELINK_AUTH_BROWSER`, `STALELINK_IGNORE_LOCAL_LINKS`, `STALELINK_IGNORE_EXCLUDE`, `STALELINK_IGNORE_EXCLUDE_URL`, `STALELINK_IGNORE_EXCLUDE_DOMAIN`, `STALELINK_FIX_WRITE`, `STALELINK_FIX_BACKUP`, `STALELINK_FIX_COPY`, and `STALELINK_OUTPUT_FAIL_ON`. Vector values use comma-separated strings, for example `STALELINK_IGNORE_EXCLUDE=generated/**,vendor/**`.

```toml
[network]
max-concurrency = 128 # per-host = 4, timeout = "20s", retries = 2, user-agent = optional string

[cache]
ttl = "24h"
dir = ".stalelink-cache" # optional; stores verdicts.sqlite3 here

[ignore]
local-links = false
exclude = ["generated/**"]
exclude-url = ["https://example.test/noisy/.*"]
exclude-domain = ["example.test"]

[output]
fail-on = "suspect"
```

TOML and environment durations use humantime syntax such as `30s`, `2h`, or `7d`; CLI `--timeout` is seconds, while `--cache-ttl` uses humantime. `[auth] auth` accepts `off`, `cookies`, or `browser`; `[auth] browser` accepts `auto`, `chrome`, `edge`, `brave`, `chromium`, or `firefox`. These auth settings, plus `[fix] write`, `backup`, and `copy`, are validated placeholders until their respective functionality lands.

The response cache is SQLite in the platform cache directory by default, with a 24-hour TTL. Set `[cache] dir` or `STALELINK_CACHE_DIR` for another location. `--no-cache` neither reads nor creates it; `--refresh` ignores prior rows while replacing them with new results. Use `stalelink cache stats` to print hits, misses, entry count, and the SQLite/WAL/SHM size, or `stalelink cache clear` to purge the local cache.

## Report formats

`stalelink scan` prints a human table by default. Use `--format json` (or `--json`) for the versioned machine-readable report, or `--format sarif` for SARIF 2.1.0. `-o <file>` writes any format to a file and leaves stdout empty. Diagnostics always go to stderr.

The JSON report has a top-level `schema_version` (currently `1`), `run`, and `findings` envelope. Version 1 is validated by the shipped [`schema/stalelink-report.v1.json`](schema/stalelink-report.v1.json). Compatible additions may be made within a schema version; incompatible changes require a new version. `run` contains files scanned, checked/unique link counts, findings grouped by confidence, and elapsed milliseconds. Cache hit/miss counters are deliberately omitted because the current cache checker seam does not expose per-run counters.

SARIF rule IDs are stable: `SL0001` HTTP status, `SL0002` network error, `SL0003` soft-404, `SL0101` login wall, `SL0201` permanent redirect, `SL0202` staleness banner, `SL0203` version drift, `SL0204` far-past last-modified, `SL0301` anomalous response, `SL0401` missing local target, and `SL0402` invalid syntax. Dead-certain findings are SARIF errors, likely-dead and outdated findings warnings, and auth-walled and suspect findings notes. Text findings include line/column regions; binary-document location data remains in result properties.

## License

MIT - see [LICENSE](LICENSE)
