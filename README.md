# stalelink

[![CI](https://github.com/jishnuteegala/stalelink/actions/workflows/ci.yml/badge.svg)](https://github.com/jishnuteegala/stalelink/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/stalelink.svg)](https://crates.io/crates/stalelink)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Your documents still link to pages that have disappeared, moved, require a login, or quietly became obsolete. `stalelink` checks local PDF, Word, Excel, PowerPoint, Markdown, HTML, and text files and reports broken links before they become support tickets or a failed audit.

It is fully local: there is no telemetry. It makes network requests only to check links you asked it to scan, and caches verdicts locally unless you choose `--no-cache`.

## Install

The first tagged release will activate the package-manager channels below. Until then, build from this checkout with stable Rust.

| Channel | Command |
| --- | --- |
| Cargo | `cargo install stalelink` (coming with first release) |
| npm | `npm install -g stalelink` (coming with first release) |
| Shell | `curl --proto '=https' --tlsv1.2 -LsSf https://github.com/jishnuteegala/stalelink/releases/latest/download/stalelink-installer.sh | sh` (coming with first release) |
| PowerShell | `irm https://github.com/jishnuteegala/stalelink/releases/latest/download/stalelink-installer.ps1 | iex` (coming with first release) |
| Source | `cargo install --path crates/stalelink` |

## Scan Links

```console
$ stalelink scan docs/
$ stalelink scan --format json docs/ > report.json
$ stalelink scan --format sarif docs/ -o stalelink.sarif
```

The default table, JSON, and SARIF outputs contain only findings on stdout. Diagnostics, progress, cookie notices, and repeatable `-v` traces go to stderr. `--quiet` suppresses progress and traces. `--color auto|always|never` controls colored fix diffs; `auto` is disabled for non-terminals and honors `NO_COLOR`.

Common errors are actionable:

- `error: path does not exist`: pass an existing file or directory.
- `error: invalid --exclude-url regex`: correct the regular expression.
- `warning: ... cookie store is unavailable`: choose another supported browser, run from an elevated Windows prompt for app-bound Chrome cookies, or use `--auth off`.
- `warning: browser tier is unavailable`: install a `live-browser` build or use `--auth cookies`.

`--auth off` uses only HTTP. The default cookie tier reads the selected browser cookie store only when an authentication-wall check escalates. `--auth browser` enables the opt-in real-browser tier when built with `live-browser`; it uses a dedicated profile or `--cdp-url`, never the default live profile.

## Fix Links

```console
$ stalelink fix handbook/                 # preview a unified diff
$ stalelink fix handbook/ --write --backup
$ stalelink fix handbook/ --copy
```

`fix` handles Markdown, text, HTML, DOCX, XLSX, PPTX, and PDF annotation links. It previews changes by default, verifies rewritten files by parsing them again, restores originals after verification failures, and refuses encrypted or signed PDFs. Use `--min-fix-confidence outdated` to include redirect suggestions.

## Configuration And Cache

`stalelink.toml` is found by walking upward from the first input path. Values resolve in this order: CLI flags, `STALELINK_*` environment variables, `stalelink.toml`, defaults.

```toml
[network]
max-concurrency = 128
per-host = 4
timeout = "20s"

[cache]
ttl = "24h"

[ignore]
exclude = ["generated/**"]

[output]
fail-on = "suspect"
```

The SQLite cache defaults to the platform cache directory. Use `stalelink cache stats`, `stalelink cache clear`, `--refresh`, or `--no-cache` to control it.

## Exit Codes

| Code | Meaning |
| --- | --- |
| 0 | Clean scan, successful cache command, or successful fix dry run/write |
| 1 | A finding meets `--fail-on`, or a fix was refused/failed |
| 2 | Invalid arguments or configuration |
| 3 | Environment or IO setup failure |

## Shell Completions

```console
stalelink completions bash > ~/.local/share/bash-completion/completions/stalelink
stalelink completions zsh > ~/.zfunc/_stalelink
stalelink completions fish > ~/.config/fish/completions/stalelink.fish
stalelink completions powershell >> $PROFILE
```

## Agent Usage

Agents can use JSON for a stable machine-readable envelope, SARIF for code-scanning integrations, and exit codes to decide whether a change is acceptable:

```console
stalelink scan --format json --fail-on likely-dead docs/ > stalelink-report.json
```

The JSON schema is [`schema/stalelink-report.v1.json`](schema/stalelink-report.v1.json). It has a strict `schema_version` of `1`; consumers should reject unknown versions rather than infer new fields.

## Release Channels

The repository is configured for draft-first GitHub Releases for macOS, Linux, and Windows on x64 and ARM64, including checksums, shell/PowerShell installers, and cargo-dist's generated `stalelink` npm installer. `release-plz` maintains the human-reviewed release PR. After a maintainer merges that PR, its post-merge `release` step publishes `stalelink-core` then `stalelink` to crates.io, creates the version tag and GitHub release, and triggers the cargo-dist workflow. The repository owner must configure the `CARGO_REGISTRY_TOKEN` Actions secret before the first crates.io release, and `NPM_TOKEN` before the generated npm installer can publish. The release workflow creates the GitHub Release as a draft; a maintainer publishes that draft after reviewing its artifacts. Homebrew, Scoop, WinGet, nFPM, and AUR files under `packaging/` are unimplemented manual examples, not release channels.

## License

MIT. See [LICENSE](LICENSE).
