# Benchmark Receipts

This directory contains the manual, reproducible language-decision benchmark
for issue #25. It is not part of the Cargo workspace, release artifacts, or CI.

- `corpus/` is generated deterministically, never committed.
- `rust-harness/` calls the production `stalelink-core` extraction API only.
- `go-port/` is a **THROWAWAY** Go comparison port, not product code.
- `run.ps1` validates identical structured extraction records before measuring.

Run `powershell.exe -NoProfile -File bench/run.ps1` from the repository root.
It records cold process completion trials and separate in-process extraction
throughput passes. Results are written to `bench/results/`.

The root Cargo gate excludes `bench/rust-harness` because it is intentionally
outside the workspace. `run.ps1` therefore runs `cargo fmt -- --check` and
Clippy with warnings denied for that harness.
