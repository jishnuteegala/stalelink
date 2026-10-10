# Fix links

Fix rescans documents, computes replacement targets for redirect and
version-drift findings, prints a preview diff by default, and can apply
changes in place (`--write`), keep `.bak` backups (`--backup`), or emit fixed
copies (`--copy`).

## Sub-features

- `fix-preview` prints a colored diff of proposed rewrites and changes
  nothing.
- `fix-write` applies rewrites at or above `--min-fix-confidence` to the
  source files.
- `fix-backup` leaves a `<name>.<ext>.bak` sibling alongside a written file
  (e.g. `guide.md.bak`).
- `fix-copy` writes `*.fixed.*` siblings instead of mutating the original.
- `fix-verify` re-reads and re-checks written targets after a write.

## How to get to it (user POV)

- Run `stalelink fix <path>` to preview.
- Run `stalelink fix --write [--backup] <path>` to rewrite files in place, or
  `stalelink fix --copy <path>` to write `*.fixed.*` siblings.
  `--write` and `--copy` conflict; pick one.

## Driving it with the shell

Preconditions:

- Doctor passed; baseline `RUN_DIR` layout and `STALELINK_CACHE_DIR` exported.
- Scratch HTTP server running per `Helpers`; port is in `scratch/port.txt`.
- A markdown fixture in `scratch/fix/` linking to
  `http://127.0.0.1:<port>/moved`, which the server 301-redirects to `/ok`;
  this yields a `PermanentRedirect` finding whose suggested fix rewrites the
  URL to the target.
- Redirect fixes carry `outdated` confidence, so write drives need
  `--min-fix-confidence outdated` (the default `dead-certain` skips them).

- **Preview.** Snapshot the fixture to `evidence/fixture.before`, then run
  `stalelink fix --no-cache scratch/fix`. stdout shows a diff hunk replacing
  `/moved` with `/ok`; the file on disk is byte-identical to the snapshot.
- **Write.** Run `stalelink fix --no-cache --write --backup
  --min-fix-confidence outdated scratch/fix`. The fixture now points at
  `/ok`, a `.bak` sibling exists; copy the result to
  `evidence/fixture.after`.
- **Read-only confirm.** Run `stalelink scan --no-cache scratch/fix` again;
  the redirect finding is gone and a plain `curl`-style check of `/ok` stays
  clean.
- **Copy mode.** On a fresh fixture, run `stalelink fix --no-cache --copy
  --min-fix-confidence outdated scratch/fix2`. The original is untouched and
  a `*.fixed.*` file holds the rewrite.

## Gotchas

- `fix` applies only findings at or above `--min-fix-confidence` (default
  `dead-certain`); redirect and version-drift fixes are `outdated`, so a
  default-confidence run previews nothing for them.
- Only findings carrying a suggested fix (`PermanentRedirect`,
  `VersionDrift`) are rewritable; dead links with no suggested target render
  as manual-fix or refused entries, not diffs.
- The written target is re-verified after a write (`fix-verify`), so a second
  scan is the honest confirmation that a rewrite stuck.
- `--copy` is a write mode, not a preview modifier: `stalelink fix --copy
  <path>` creates `*.fixed.*` files with no `--write`, and clap rejects
  `--write --copy` together (exit 2).
- Backup files (`*.bak`) inside the scanned tree get re-scanned; keep
  evidence copies outside `scratch/`.
