# stalelink

Scan documents for dead and outdated links.

`stalelink` walks your PDFs, Word/Excel/PowerPoint files, Markdown, HTML, and plain text, extracts every link, and tells you which ones are dead, redirected, login-walled, or stale - including links behind authentication, checked with your own browser's logged-in state so they are never falsely reported dead.

Fully local and open source: no telemetry, no data stored or tracked by the CLI.

> Status: pre-release - under active development. The feature set below describes what is being built; the first tagged release ships the complete local feature set.

## Planned surface

- **Formats:** PDF, DOCX, XLSX, PPTX, Markdown, HTML, plain text
- **Checks:** HTTP status, soft-404s, redirect chains, login walls, staleness signals (deprecated banners, versioned-URL drift), local file paths, intra-document anchors, cross-document relative links
- **Auth tiers:** plain HTTP -> browser-profile cookies -> real browser (CDP) for suspect links (Chrome, Edge, Brave, Chromium, Firefox)
- **Output:** human table, `--json`, SARIF; confidence levels (dead-certain, likely-dead, auth-walled, outdated, suspect); suggested replacement URLs; CI-friendly exit codes
- **Auto-fix:** `stalelink fix` previews link rewrites as a diff; `--write` applies them, across every supported format
- **Performance:** parallel parsing, URL dedupe, response caching, polite per-host rate limits

## Local links

Explicit Markdown destinations, HTML `href`/`src` attributes, and external PDF/OOXML targets may point to local files. Relative paths resolve from the containing document; absolute/root-relative paths resolve from the filesystem root. Percent escapes in paths are decoded before lookup. Markdown headings use lowercase slugs with punctuation removed and whitespace collapsed to hyphens; Markdown and HTML also recognize `id` and `<a name>` anchors. Anchors in other existing formats are not inspected.

`mailto:` requires a simple `local@domain.tld` shape. `tel:` requires at least one digit and permits only digits, `+`, `-`, parentheses, and spaces. Use `--no-local` to skip all local, mailto, and tel checks. Config-based local-link ignores are not yet implemented.

## License

MIT - see [LICENSE](LICENSE)
