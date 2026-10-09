# stalelink verification map

This directory is the maintained source for verifying stalelink's user-facing
CLI behavior.
Read the baseline before driving, then use the matching feature file as the
recipe.

## Baseline preconditions

- Build once: `cargo build -p stalelink`; the binary is
  `target/debug/stalelink` (`.exe` on Windows) or an installed `stalelink` on
  `PATH` when proving a shipped channel.
- Create a per-run directory `RUN_DIR` with `scratch/` and `evidence/`
  subdirectories, as defined in `../SKILL.md`.
- Export `STALELINK_CACHE_DIR="$RUN_DIR/scratch/cache"` so no run shares the
  platform cache.
- Build all fixtures inside `scratch/`; never point the CLI at real
  repository or home files.
- Pass the doctor check in `../SKILL.md` before the first drive.

## Driving conventions

- Start every recipe from the baseline unless its preconditions say otherwise.
- Drive with the subcommand and long flags exactly as written; exit codes are
  part of the contract: 0 clean (or all fixes applied), 1 findings reported or
  fix work refused/failed, 2 usage error, 3 environment/execution failure.
- Use `--format json` when a recipe needs machine-checkable output and `-o`
  when it needs a written artifact; `--no-cache` whenever caching is not the
  feature under test.
- HTTP liveness drives use the scratch `node` server documented under
  `Helpers` in `../SKILL.md`; record its PID at launch and stop it at cleanup.
- Treat every command literally; quote paths and globs.

## Proof and skip reporting

- Evidence is command transcript (command, stdout, stderr, exit code) plus
  output artifacts (`-o` files, before/after fixture copies).
- A mutation (`fix --write`) needs a read-only second look at the changed
  file to count as verified.
- Record the feature ID and the entry point used with every artifact.
- Report an unreachable path with the attempted command and the unmet
  precondition; never report a skipped path as verified elsewhere.
- Cleanup removes `scratch/` and stops helper processes; `evidence/` always
  survives.

## Feature entry contract

Each feature file opens with an H1 and one paragraph on the user-visible
behavior, then exactly four H2 sections in order:

1. `Sub-features` - short IDs, one line per behavior.
2. `How to get to it (user POV)` - every user entry point.
3. `Driving it with <harness>` - `Preconditions:` then labeled action/result
   bullets pairing the user action with the exact command and observable
   result.
4. `Gotchas` - traps that waste or invalidate a run.

## Features

- [Scan documents](./scan.md) - walk a directory of documents, check links,
  report findings on stdout or as JSON/SARIF, and gate CI via exit code.
- [Fix links](./fix.md) - preview suggested fixes as a diff, apply them in
  place or to copies, with optional backups.
- [Verdict cache](./cache.md) - inspect and clear the HTTP-verdict cache that
  speeds up repeated scans.
- [Shell completions](./completions.md) - generate completion scripts for
  bash, zsh, fish, and PowerShell.
