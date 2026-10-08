# stalelink

[![CI](https://github.com/jishnuteegala/stalelink/actions/workflows/ci.yml/badge.svg)](https://github.com/jishnuteegala/stalelink/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/stalelink.svg)](https://crates.io/crates/stalelink)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Your documents still link to pages that have disappeared, moved, require a login, or quietly became obsolete. `stalelink` checks local PDF, Word, Excel, PowerPoint, Markdown, HTML, and text files and reports broken links before they become support tickets or a failed audit.

It is fully local and collects no telemetry.
It makes network requests only to check links you asked it to scan, and caches verdicts locally unless you choose `--no-cache`.

## Install

```sh
# Shell installer (macOS / Linux)
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/jishnuteegala/stalelink/releases/latest/download/stalelink-installer.sh | sh

# PowerShell installer (Windows)
irm https://github.com/jishnuteegala/stalelink/releases/latest/download/stalelink-installer.ps1 | iex

# Cargo
cargo install stalelink

# npm / bun / pnpm (or one-off: npx @jishnuteegala/stalelink scan docs/)
npm install -g @jishnuteegala/stalelink

# Homebrew (macOS / Linux)
brew install jishnuteegala/tap/stalelink

# Chocolatey (Windows)
choco install stalelink
```

The Scoop, WinGet, and AUR channels are built into the release pipeline but are not published yet.

### Linux packages

`stalelink` is not in the official Debian/Fedora/etc. archives, so plain `apt install stalelink` won't work. Instead, every release attaches native packages you download and install manually. For example (amd64; replace `amd64` with `arm64` for ARM):

```sh
VERSION=$(curl -s https://api.github.com/repos/jishnuteegala/stalelink/releases/latest | grep -Po '"tag_name": "v\K[^"]*')

# Debian / Ubuntu
curl -LO "https://github.com/jishnuteegala/stalelink/releases/download/v${VERSION}/stalelink-${VERSION}-amd64.deb"
sudo dpkg -i "stalelink-${VERSION}-amd64.deb"

# Fedora / RHEL
sudo dnf install "https://github.com/jishnuteegala/stalelink/releases/download/v${VERSION}/stalelink-${VERSION}-amd64.rpm"

# Alpine
curl -LO "https://github.com/jishnuteegala/stalelink/releases/download/v${VERSION}/stalelink-${VERSION}-amd64.apk"
sudo apk add --allow-untrusted "stalelink-${VERSION}-amd64.apk"
```

`.pkg.tar.zst` (Arch) packages are also attached to each [release](https://github.com/jishnuteegala/stalelink/releases).
Manual installs do not update themselves, so use Homebrew, npm, or a package manager channel when you want upgrades handled for you.

Prebuilt binary archives are also on the Releases page.
The build matrix covers Linux, macOS, and Windows on x86_64 + aarch64.
Each release contains these checksummed payloads (replace `${VERSION}` with the release version):

```text
stalelink-x86_64-unknown-linux-gnu.tar.xz
stalelink-aarch64-unknown-linux-gnu.tar.xz
stalelink-x86_64-apple-darwin.tar.xz
stalelink-aarch64-apple-darwin.tar.xz
stalelink-x86_64-pc-windows-msvc.zip
stalelink-aarch64-pc-windows-msvc.zip
stalelink-${VERSION}-amd64.deb
stalelink-${VERSION}-arm64.deb
stalelink-${VERSION}-amd64.rpm
stalelink-${VERSION}-arm64.rpm
stalelink-${VERSION}-amd64.apk
stalelink-${VERSION}-arm64.apk
stalelink-${VERSION}-amd64.pkg.tar.zst
stalelink-${VERSION}-arm64.pkg.tar.zst
sha256.sum
```

## Scan links

```console
$ stalelink scan docs/
$ stalelink scan --format json docs/ > report.json
$ stalelink scan --format sarif docs/ -o stalelink.sarif
```

The default table, JSON, and SARIF outputs contain only findings on stdout.
Diagnostics, progress, cookie notices, and repeatable `-v` traces go to stderr.
`--quiet` suppresses progress and traces.
`--color auto|always|never` controls colored fix diffs.
`auto` turns color off for non-terminals and honors `NO_COLOR`.

Common errors are actionable:

- `error: path does not exist`: pass an existing file or directory.
- `error: invalid --exclude-url regex`: correct the regular expression.
- `warning: ... has no readable cookie store`: check the browser profile or use `--auth off`.
- `warning: ... cookie store is unavailable`: close the browser or choose another supported browser; on Windows, an elevated prompt may be needed for app-bound Chrome cookies.
- `warning: browser tier is unavailable`: install a `live-browser` build or use `--auth cookies`.

`--auth off` uses only HTTP. The default cookie tier reads the selected browser cookie store only when an authentication-wall check escalates. `--auth browser` enables the opt-in real-browser tier when built with `live-browser`; it uses a dedicated profile or `--cdp-url`, never the default live profile.

## Fix links

```console
$ stalelink fix handbook/                 # preview a unified diff
$ stalelink fix handbook/ --write --backup
$ stalelink fix handbook/ --copy
```

`fix` handles Markdown, text, HTML, DOCX, XLSX, PPTX, and PDF annotation links. It previews changes by default, verifies rewritten files by parsing them again, restores originals after verification failures, and refuses encrypted or signed PDFs. Use `--min-fix-confidence outdated` to include redirect suggestions.

## Configuration and cache

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

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Clean scan, successful cache command, or successful fix dry run/write |
| 1 | A finding meets `--fail-on`, or a fix was refused/failed |
| 2 | Invalid arguments or configuration |
| 3 | Environment or IO setup failure |

## Shell completions

```console
stalelink completions bash > ~/.local/share/bash-completion/completions/stalelink
stalelink completions zsh > ~/.zfunc/_stalelink
stalelink completions fish > ~/.config/fish/completions/stalelink.fish
stalelink completions powershell >> $PROFILE
```

## Agent usage

Agents get a stable machine-readable JSON envelope, SARIF for code-scanning integrations, and exit codes driven by `--fail-on` to gate a change:

```console
stalelink scan --format json --fail-on likely-dead docs/ > stalelink-report.json
```

The JSON schema is [`schema/stalelink-report.v1.json`](schema/stalelink-report.v1.json). It has a strict `schema_version` of `1`; consumers should reject unknown versions rather than infer new fields.

## Release channels

Releases are draft-first GitHub Releases for macOS, Linux, and Windows on x64 and ARM64, with checksums, shell/PowerShell installers, cargo-dist's generated `stalelink` npm installer, a Homebrew tap, Scoop, nFPM Linux packages, AUR, WinGet, and Chocolatey.
`release-plz` maintains the human-reviewed release PR.
On merge it publishes `stalelink-core` then `stalelink` to crates.io, creates the version tag, and dispatches cargo-dist.
Cargo-dist creates the draft and uploads artifacts; Homebrew and nFPM complete before cargo-dist undrafts it.
Scoop, AUR, WinGet, and Chocolatey then use the public release URLs.
Optional credential-backed channels skip when their secrets are absent.

## License

MIT. See [LICENSE](LICENSE).
