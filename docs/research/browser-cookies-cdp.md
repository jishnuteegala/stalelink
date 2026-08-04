# Browser Cookie Extraction (Tier 2) and CDP Real-Profile Attach (Tier 3)

Date: 2026-08-04
Issue: https://github.com/jishnuteegala/stalelink/issues/4
Scope: Research only, grounded in primary sources (crates.io API, docs.rs, GitHub source, Chromium/Firefox docs).

## Question

Tier 2: how to read cookies from Chrome/Edge/Brave/Chromium (Windows DPAPI plus v10/v20 app-bound encryption from Chrome 127+, macOS Keychain, Linux kwallet/gnome-keyring) and Firefox (cookies.sqlite) in Rust.
Which crates exist, how they handle app-bound cookie encryption as of 2026, and how they cope with locked databases while the browser runs.

Tier 3: which CDP crate can attach to or launch the user's real profile for checking suspect links, and the profile-lock constraints.

Deliver: per-OS/per-browser feasibility matrix, crate recommendation for tier 2 and tier 3, and known failure modes to surface to users.

## Tier 2 - Cookie Extraction

### Encryption background per platform

Windows (Chromium family), pre-127:
Cookie values in the `Cookies` SQLite DB (Network/Cookies) are prefixed `v10` and AES-256-GCM encrypted.
The AES key is stored in `Local State` JSON under `os_crypt.encrypted_key`, itself DPAPI-protected (user-scoped, `CryptUnprotectData`).
Any process running as the logged-in user can read it. This is the classic infostealer path.

Windows (Chromium family), Chrome 127+ App-Bound Encryption (ABE):
Google's July 2024 announcement (security.googleblog.com/2024/07/improving-security-of-chrome-cookies-on.html) introduced App-Bound Encryption, migrating cookies first.
A privileged `elevation_service.exe` running as SYSTEM encodes the requesting app's identity into the key and verifies it on decrypt; another app fails.
The `Local State` key is now `os_crypt.app_bound_encrypted_key`, base64 with an `APPB` prefix, and new cookies carry a `v20` prefix.
Decrypting the app-bound key requires SYSTEM-level DPAPI (first layer) then user DPAPI (second layer), then an additional AES-256-GCM unwrap using a key baked into `elevation_service.exe`.
Net effect: a plain user-space process can no longer read v20 cookies without either admin/SYSTEM elevation or code injection into Chrome.
Rollout has widened across Chrome/Edge/Brave through 2025-2026; on current stable builds new cookies are predominantly v20.

macOS (Chromium family):
The AES key ("Chrome Safe Storage" / "Chromium Safe Storage" / per-browser entry) lives in the login Keychain.
Reading it triggers a Keychain access-control prompt unless the calling binary is already trusted for that item.
Cookie values are `v10` AES-128-CBC (PBKDF2 from the keychain secret). No ABE equivalent on macOS; the Keychain already provides app-bound semantics.

Linux (Chromium family):
Key stored via Secret Service - GNOME Keyring (libsecret) or KWallet - under "Chrome Safe Storage".
If no keyring is available or the "basic" storage backend is used, Chromium falls back to a hardcoded password `peanuts`, making cookies trivially decryptable.
Cookie values are `v10`/`v11` AES-128-CBC (PBKDF2). No ABE on Linux.

Firefox (all platforms):
Cookies are stored unencrypted in `cookies.sqlite` in the profile directory (table `moz_cookies`).
No OS keychain or DPAPI involved; only the SQLite file lock matters. This is the easiest tier-2 target on every OS.

### Rust crates surveyed (crates.io API, 2026-08-04)

rookie (github.com/thewh1teagle/rookie):
Latest 0.5.6, updated 2024-11-01, ~72k downloads, ~11k recent.
Multi-language project (Rust/Python/JS); Rust crate is `rookie`.
Supports Arc, Brave, Chrome, Chromium, Edge, Firefox, LibreWolf, Opera, Opera GX, Safari, Vivaldi, Zen, IE across Windows/macOS/Linux.
Only surveyed crate that explicitly implements App-Bound (v20) decryption.

bench_scraper (github.com/goakley/bench_scraper):
Latest 0.4.1, updated 2022-12-03, ~12k downloads. Effectively unmaintained.
Predates Chrome 127 ABE (2024); cannot handle v20 cookies. DPAPI/v10 era only. Not viable for current Chromium.

Other options:
Rolling your own is feasible for the easy cases: `rusqlite` (open cookies DB), plus `aes-gcm`/`aes` + `pbkdf2` for value decryption, `windows`/`windows-sys` for DPAPI (`CryptUnprotectData`), `security-framework` for macOS Keychain, `libsecret`/`secret-service` for Linux.
This is the path if we want to avoid a heavy dependency and control failure messaging precisely - but it means re-implementing the v20 SYSTEM-impersonation dance ourselves.

### How rookie handles App-Bound (v20) - from source

rookie's `rookie-rs/src/windows/appbound/mod.rs` cites the reference implementation runassu/chrome_v20_decryption.
The v20 key (base64, `APPB` prefix) is unwrapped in three stages:
1. Decrypt with SYSTEM DPAPI - rookie impersonates SYSTEM via `appbound/impersonate.rs` (token impersonation), which requires the process to already be elevated/admin.
2. Decrypt the result again with user DPAPI (`windows/dpapi.rs`, `CryptUnprotectData`).
3. Take the trailing key bytes; for Chrome, additionally AES-256-GCM-unwrap using a hardcoded key extracted from `elevation_service.exe` (embedded in rookie as a base64 constant), yielding the final 32-byte AES key.
The README states plainly: bypassing Chrome file locking and app-bound encryption "requires admin rights on Windows from v130.x".
So rookie does solve v20, but only when stalelink itself runs elevated. Without elevation, v20 cookies remain unreadable.

### Locked-database handling while the browser is running

The `Cookies` SQLite file is held with a Windows exclusive/share lock while Chromium runs; naive `rusqlite` opens fail with "database is locked" or a sharing violation.
Firefox uses WAL mode and is generally more tolerant, but concurrent writes can still surface `SQLITE_BUSY`.

Approaches, in order of intrusiveness:
1. SQLite copy trick: copy the DB (and `-wal`/`-shm` sidecars) to a temp path, then open the copy read-only. Works on macOS/Linux where files are not exclusively locked; on Windows the exclusive lock can block a plain `std::fs::copy`.
2. Immutable / read-only open: open with SQLite URI `file:...?immutable=1` or `mode=ro`, bypassing locking. Reads a possibly slightly stale DB but avoids lock contention. Good default for a read-only scanner.
3. Windows raw copy (rookie's approach): `rookie-rs/src/windows/shadow_copy.rs` uses `rawcopy-rs-next` to read the file's raw NTFS bytes past the exclusive lock. This requires admin rights (`privilege::user::privileged()` guard) and effectively performs a low-level volume read.
4. Ask the user to close the browser: simplest and most reliable, but poor UX for a background scanner.

For stalelink (read-only scanner) the pragmatic default is: try immutable/read-only open first, fall back to copy-then-open, and only use raw copy when already elevated.

### Per-OS / per-browser feasibility matrix (cookie extraction)

Legend: Easy = no elevation, no prompt. Prompt = OS credential/keychain prompt likely. Elevated = needs admin/SYSTEM. Blocked = not readable in that mode.

| Browser (engine) | Windows (pre-127 v10) | Windows (127+ v20, no elevation) | Windows (127+ v20, elevated) | macOS | Linux (keyring) | Linux (basic/peanuts) |
| --- | --- | --- | --- | --- | --- | --- |
| Chrome (Chromium)   | Easy | Blocked | Elevated (rookie) | Prompt (Keychain) | Prompt (Secret Service) | Easy |
| Edge (Chromium)     | Easy | Blocked | Elevated (rookie) | Prompt (Keychain) | Prompt (Secret Service) | Easy |
| Brave (Chromium)    | Easy | Blocked | Elevated (rookie) | Prompt (Keychain) | Prompt (Secret Service) | Easy |
| Chromium            | Easy | Blocked | Elevated (rookie) | Prompt (Keychain) | Prompt (Secret Service) | Easy |
| Firefox (Gecko)     | Easy | Easy (no ABE)       | Easy             | Easy   | Easy            | Easy  |

Notes:
Windows v10 cookies remain readable without elevation even on a 127+ browser, but new cookies are written as v20, so coverage degrades over time without elevation.
macOS/Linux "Prompt" becomes "Easy" on repeat runs if the user grants persistent access (Keychain "Always Allow" / keyring unlock).
Firefox is uniformly Easy because cookies.sqlite is plaintext; only the file lock is a concern (handled by copy/immutable open).

## Tier 3 - CDP Real-Profile Attach / Launch

### Crate comparison (crates.io API, 2026-08-04)

chromiumoxide (github.com/mattsse/chromiumoxide):
Latest 0.9.1, updated 2026-02-25. ~3.19M downloads, ~1.45M recent. Edition 2024, rustc 1.85.
Async (tokio), full CDP surface generated from the PDL, actively maintained.
Supports launching headless or full Chrome/Chromium AND connecting to an already-running instance (`Browser::connect(debug_ws_url)`).
Downside: pulls in tokio and ~60K lines of generated CDP code (slow first build).

headless_chrome (github.com/rust-headless-chrome/rust-headless-chrome):
Latest 1.0.22, updated 2026-06-11. ~2.82M downloads, ~897k recent. Synchronous/blocking API.
Mature, simpler surface, no async runtime required. Can launch and can connect to an existing instance via websocket URL.
Downside: smaller CDP surface than chromiumoxide's generated bindings; less ergonomic for exotic commands.

Both are healthy in 2026. chromiumoxide is the more complete and more actively developed; headless_chrome is lighter and sync.

### Profile-lock constraints

Chrome/Chromium enforces a `SingletonLock` (a lock file / named mutex) on a profile directory: a second Chrome process cannot open a profile already in use.
This is the central constraint for "check a suspect link with the user's real cookies".

Options:
1. Attach to a running instance via `--remote-debugging-port`:
   If the user's Chrome is already started with `--remote-debugging-port=9222`, connect over CDP (`http://127.0.0.1:9222/json/version` gives the ws endpoint) and drive existing tabs.
   Pros: real profile, real cookies, no lock conflict. Cons: the user must have launched Chrome with the flag; you cannot enable it on an already-running process. Recent Chrome also restricts remote debugging on the Default profile for security (requires `--user-data-dir` in some builds), which limits this path.
2. Temporary profile copy:
   Copy the profile dir to a temp location and launch a second Chrome against the copy with `--user-data-dir`.
   Pros: no lock conflict with the live browser. Cons: large copy, cookies may be v20-encrypted and thus not portable across machines/profiles, and copying a live profile risks corruption/staleness.
3. Dedicated stalelink profile:
   Create a separate `--user-data-dir` that the user logs into once; stalelink launches Chrome against that profile on demand.
   Pros: clean, no lock conflict, no elevation, cookies belong to a profile stalelink controls. Cons: one-time manual login per site; not the user's "real" session automatically.
4. Ask the user to close Chrome, then launch against the real profile dir:
   Simple, gives real cookies, but disrupts the user and risks profile writes.

### Recommended Tier 3 approach

Primary: chromiumoxide, connecting to a user-started `--remote-debugging-port` instance when available (zero lock conflict, real session).
Fallback: a dedicated stalelink `--user-data-dir` profile the user authenticates once, launched on demand.
Avoid copying the live Default profile as a default path (corruption/staleness/v20 non-portability).
If a lighter, sync dependency is strongly preferred and the CDP needs are modest, headless_chrome is an acceptable substitute.

## Failure Modes to Surface to Users

App-bound (v20) cookies unreadable without elevation (Windows, Chrome 127+):
Without admin/SYSTEM, v20 cookies cannot be decrypted. Surface a clear message: "Newer Chrome cookies are protected by App-Bound Encryption; run stalelink as administrator to read them, or use Tier 3 (real browser)."
Do not silently return partial results; report how many cookies were skipped as v20.

Browser running locks the cookie DB (Windows):
Naive open fails with a sharing violation / "database is locked". Surface: "Chrome is running and is locking its cookie database; close Chrome, or stalelink will retry with a read-only snapshot." Prefer immutable/read-only open then copy fallback before asking the user to close.

Keychain / Secret Service prompts (macOS, Linux):
Reading the encryption key triggers an OS prompt. Surface: "Your OS will ask permission to read the browser's saved key; grant it (choose Always Allow to avoid repeat prompts)." If denied, report the browser as skipped, not failed silently.
On Linux with the "basic" backend, cookies use the hardcoded `peanuts` password - readable but warn that these were weakly protected.

Elevation required but not granted:
rookie's raw-copy and SYSTEM-impersonation paths bail with "No admin rights". Surface this explicitly and offer the non-elevated subset (v10 + Firefox) rather than aborting the whole scan.

CDP attach failures (Tier 3):
No `--remote-debugging-port` instance found, or Chrome refuses a second process on the locked profile ("profile appears to be in use"). Surface the dedicated-profile fallback instead of failing.
Recent Chrome restricting remote debugging on the Default profile can break option 1 even when the flag is set.

Portability of v20 keys:
v20-encrypted cookies are bound to the machine/user; copying a profile to another machine yields undecryptable cookies. Do not rely on copied profiles for Tier 3 auth.

## Final Recommendation

Tier 2 (cookie extraction):
Use rookie as the primary crate - it is the only surveyed Rust crate that implements App-Bound (v20) decryption, and it covers all target browsers across Windows/macOS/Linux.
Run non-elevated by default (covers Firefox everywhere, plus v10 Chromium cookies); detect v20 and, only when the user opts in, re-run elevated so rookie's SYSTEM-impersonation + elevation_service.exe key unwrap can read v20.
For locked DBs, prefer read-only/immutable open, fall back to copy-then-open, and use rookie's raw copy only when already elevated.
Reconsider a hand-rolled implementation (rusqlite + aes-gcm + DPAPI/security-framework/secret-service) only if we need tighter control over failure messaging or want to drop the dependency; the v20 path is the costly part to reproduce.
Reject bench_scraper (unmaintained since 2022, no v20 support).

Tier 3 (real-browser CDP):
Use chromiumoxide (actively maintained, full CDP, async/tokio).
Connect to a user-started `--remote-debugging-port` instance when present; otherwise launch a dedicated stalelink `--user-data-dir` profile the user logs into once.
Never launch a second process against the live Default profile (SingletonLock) and never depend on copied profiles for v20 cookies.
headless_chrome is the acceptable lighter, synchronous alternative if async/tokio is unwanted.

Sources:
security.googleblog.com/2024/07/improving-security-of-chrome-cookies-on.html (App-Bound Encryption announcement).
github.com/thewh1teagle/rookie (source: windows/appbound/mod.rs, shadow_copy.rs, dpapi.rs; README support matrix).
github.com/runassu/chrome_v20_decryption (v20 reference algorithm cited by rookie).
github.com/mattsse/chromiumoxide and github.com/rust-headless-chrome/rust-headless-chrome (CDP crates).
crates.io API for rookie, bench_scraper, chromiumoxide, headless_chrome (version/maintenance data, retrieved 2026-08-04).
