# AGENTS.md

Rust CLI that scans local documents for dead and outdated links.
Toolchain: Rust stable (MSVC on Windows). Full local gate: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` (the `check` alias in `.cargo/config.toml` runs all three).

## Agent skills

### Issue tracker

Issues are tracked on GitHub Issues for this repo (chosen for the long-running multi-session build: native blocking relationships and `Closes #N` reaping). See `docs/agents/issue-tracker.md`.

### Triage labels

Default five-role vocabulary (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `CONTEXT.md` at the repo root, ADRs under `docs/adr/`. See `docs/agents/domain.md`.
