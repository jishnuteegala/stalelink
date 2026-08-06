# Benchmark Receipts

This directory contains the manual, reproducible language-decision benchmark for
issue #25. It is not part of the Cargo workspace, release artifacts, or CI.

- `corpus/` is generated, never committed.
- `rust-harness/` links the production `stalelink-core` extraction API only.
- `go-port/` is a **THROWAWAY** Go comparison port, not product code.
- `run.ps1` generates the corpus, builds both programs, and records Windows
  peak working set, cold-start, and steady-state throughput.

Run `powershell.exe -NoProfile -File bench/run.ps1` from the repository root. Results are written to
`bench/results/`; copy the measured values into `docs/benchmarks.md` when
refreshing the receipt.
