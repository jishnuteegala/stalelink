# Verdict cache

Cache inspects and clears the SQLite verdict cache that stores HTTP check
results between runs, so repeated scans skip already-checked URLs inside
their TTL window.

## Sub-features

- `cache-stats` reports `hits`, `misses`, `entries`, and `size` for the
  cache.
- `cache-clear` deletes the cached verdicts.
- `cache-effect` makes a second scan avoid re-probing already-checked URLs.

## How to get to it (user POV)

- Run `stalelink cache stats` to inspect.
- Run `stalelink cache clear` to empty.

## Driving it with the shell

Preconditions:

- Doctor passed; `STALELINK_CACHE_DIR` points at `scratch/cache` so the drive
  exercises a throwaway cache, never the platform one.
- Scratch HTTP server running per `Helpers`; port is in `scratch/port.txt`.
- A fixture with at least one HTTP URL against the scratch server.

- **Populate.** Run `stalelink scan scratch/docs` (no `--no-cache`) so
  verdicts are written into `scratch/cache/verdicts.sqlite3`.
- **Stats.** Run `stalelink cache stats`. stdout reports the scratch cache
  location and non-zero entry counts.
- **Effect.** Rescan the same fixture; the run completes without re-probing
  the scratch server (server log shows no new requests).
- **Clear.** Run `stalelink cache clear`, then `stalelink cache stats`.
  Stats reports an empty cache; a third scan re-probes the server.

## Gotchas

- Forgetting `STALELINK_CACHE_DIR` makes `stats`/`clear` operate on the real
  platform cache; set it before any cache drive.
- `--no-cache` bypasses both read and write, so a drive launched with it
  never populates the cache even when the cache feature itself is under test.
- `--refresh` ignores reads but still writes; it is not a substitute for a
  cold cache.
- TTL filtering (`--cache-ttl`, `STALELINK_CACHE_TTL`) can make entries look
  absent while they still exist on disk.
