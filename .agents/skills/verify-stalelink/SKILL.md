---
name: verify-stalelink
description: Verify user-facing behavior of stalelink's CLI (scan, fix, cache, completions) by launching a real binary, driving feature-map recipes, capturing transcripts and output files as evidence, and cleaning up scratch state. Use for /verify-stalelink, "verify the CLI", or "prove this feature works" on this repository.
---

# Verify stalelink

Prove stalelink's user-facing features work by driving the CLI's real paths and
capturing evidence a reviewer can trust.

Read `features/README.md` for baseline preconditions and driving conventions, then
follow the matching feature file as the recipe for each drive.

## Launch

- Build the binary once per session: `cargo build -p stalelink`.
  The binary lands at `target/debug/stalelink` (`target/debug/stalelink.exe` on
  Windows). A freshly installed `stalelink` on `PATH` is an acceptable substitute
  when the goal is proving an installed channel rather than the checkout.
- stalelink is a short-lived CLI: every drive is one process invocation against
  scratch input. There is no server, daemon, port, or instance to keep alive.
- Per-run isolation contract:
  - Give every verification run its own run directory:
    `RUN_DIR="${TMPDIR:-/tmp}/stalelink-verify-$(date +%Y%m%d%H%M%S)"` with
    `scratch/` for inputs and `evidence/` for artifacts.
  - Point the cache at the scratch dir so runs never share the platform cache:
    `export STALELINK_CACHE_DIR="$RUN_DIR/scratch/cache"`.
  - For drives that need an HTTP endpoint, start a scratch server on a free port
    under `scratch/` and stop it in cleanup (see `Helpers`).
- Teardown is per-invocation; nothing persists between commands except the
  scratch cache you pointed at.

## Doctor

Run a read-only health check before any drive:

```sh
BIN=./target/debug/stalelink    # or `stalelink` when testing an installed channel
"$BIN" --version                # expect `stalelink <semver>` matching Cargo.toml
"$BIN" scan --help >/dev/null   # expect exit 0; confirms clap tree is usable
"$BIN" completions bash | head -1   # expect a completion script line
mkdir -p "$RUN_DIR/scratch" "$RUN_DIR/evidence"
echo doctor-ok > "$RUN_DIR/evidence/doctor.txt"
```

Doctor passes when `--version` prints a semver matching `crates/stalelink/Cargo.toml`,
`scan --help` exits 0, and a completion script emits output. A failed doctor means
the build or checkout is broken; do not drive.

## Drive

- Drive only through the CLI surface: `stalelink scan`, `stalelink fix`,
  `stalelink cache`, `stalelink completions`.
  No internal APIs, test hooks, or environment state poking beyond the
  documented `STALELINK_*` env vars.
- Prefer stable names: subcommands and long flags, not output column positions
  or terminal coordinates.
- Every drive runs a real user path against `scratch/` fixtures you create;
  never scan or fix files outside `scratch/`.
- Pass `--no-cache` on drives that do not test caching, and `STALELINK_CACHE_DIR`
  on ones that do.
- Capture each invocation's stdout, stderr, and exit code as it happens; exit
  codes are part of the contract (0 = clean, 1 = findings/failure, 2 = usage,
  3 = environment/execution failure).

## Evidence

- Write every artifact under `$RUN_DIR/evidence/`: command transcripts
  (command + stdout + stderr + exit code), `--format json` or `-o` SARIF output
  files, and copies of fixture files before and after a `fix --write` drive.
- A drive is proven only when the evidence shows both the user action and the
  resulting state: the command that ran and the exit code, output, or file
  mutation it produced.
- Mutation proof needs a read-only second look: after `fix --write`, diff the
  before/after copies or re-read the file to confirm the link was rewritten.
- If a path is unreachable (network absent, auth tier unavailable), record the
  attempted command and the unmet precondition as evidence; do not report it
  verified through a different path.
- The evidence directory must survive `Cleanup`.

## Cleanup

- Stop the scratch HTTP server if one was started (record its PID at launch;
  on Windows use `Stop-Process -Id <pid>`; elsewhere `kill <pid>`).
- Remove `$RUN_DIR/scratch/` only. Never delete `$RUN_DIR/evidence/`.
- Do not kill processes by name; a correctly launched run owns exactly one
  optional helper PID.
- Never touch the real platform cache: only the `STALELINK_CACHE_DIR` under
  `scratch/` may be removed.
- After cleanup, confirm `ls "$RUN_DIR/evidence"` still lists artifacts.

## Helpers

- Scratch HTTP endpoint: a local server is the only way to prove live-status
  checking offline. One-liner, started in the background with its log and PID
  captured:

  ```sh
  node -e 'require("http").createServer((q,s)=>{if(q.url==="/moved"){s.writeHead(301,{Location:"/ok"});s.end()}else{s.statusCode=q.url==="/dead"?404:200;s.end("ok")}}).listen(0,"127.0.0.1",function(){console.log(this.address().port)})' \
    > "$RUN_DIR/scratch/port.txt" & echo $! > "$RUN_DIR/scratch/server.pid"
  ```

  It 301-redirects `/moved` to `/ok`, returns 404 on `/dead`, and 200
  everywhere else; the chosen port lands in `port.txt`.
  Wait for `port.txt` to be non-empty before driving against it.
- Evidence transcript helper (per drive):

  ```sh
  run_capture() { # label, then command...
    local label="$1"; shift
    { echo "+ $*"; "$@" >"$RUN_DIR/evidence/$label.stdout" 2>"$RUN_DIR/evidence/$label.stderr"; echo "exit=$?"; } \
      > "$RUN_DIR/evidence/$label.transcript" 2>&1
  }
  ```

Keep this skill honest: run `/maintain-verification-skill` when CLI flags,
subcommands, or feature behavior change.
