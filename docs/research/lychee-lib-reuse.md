# Research: lychee-lib reuse boundary for stalelink tier-1 HTTP checking

Date: 2026-08-04
Question: How much of lychee-lib can stalelink reuse for tier-1 HTTP link checking, and where does stalelink's polite-by-default policy diverge from what lychee-lib offers?
Issue: https://github.com/jishnuteegala/stalelink/issues/3
Sources examined: docs.rs/lychee-lib 0.24.2 (published 2026-07-11) and the lycheeverse/lychee master branch source on GitHub.

## lychee-lib API surface (0.24.2)

The library entry point is `ClientBuilder` (typed-builder), which produces a `Client` with a `check(request) -> Result<Response>` method.
`Response` wraps a `Status` enum: `Ok(StatusCode)`, `Error(ErrorKind)`, `Timeout(Option<StatusCode>)`, `RequestError`, `UnknownStatusCode`, `Excluded`, `Unsupported`, `Cached(CacheStatus)`.
`ErrorKind` is a rich thiserror enum (network, TLS, builder, GitHub, body-read errors); docs warn not to match on message text.

`ClientBuilder` options relevant to tier-1 HTTP checking:
- `method(Methods)`: an ordered list of HTTP methods; default is `GET` only, but `Methods::try_from(vec![Method::HEAD, Method::GET])` gives HEAD-to-GET fallback.
- `max_retries` (default 3, `DEFAULT_MAX_RETRIES`), `retry_wait_time` (default 1s, doubles per retry: 2^(N-1) pattern in `client.rs`).
- `timeout` (default 20s), `max_redirects` (default constant), `user_agent` (default `lychee-<version>`), `custom_headers`, `min_tls_version`, `allow_insecure`, `schemes`, `accepted` (StatusCodeSelector), `require_https`, `cookie_jar`, `remaps`, `includes`/`excludes` filters, `exclude_all_private` and friends.
- `rate_limit_config(RateLimitConfig)` and `hosts(HostConfigs)`: this is the per-host rate limiting layer (new-ish; the `ratelimit` module).
- `basic_auth`, `github_token` (octocrab), `plugin_request_chain` (chain-of-responsibility request middleware), `fragment_checker_options`.

### Method fallback behavior (checker/website.rs)

`check_website_inner` tries each configured method in order and returns the first success.
It deliberately falls back on any error response (404, 403, 405, connection reset) without inspecting the reason, except timeouts, which short-circuit because a heavier method would only take longer.
Each method attempt goes through `retry_request`, which retries up to `max_retries` with exponential doubling of `retry_wait_time`, gated on `Status::should_retry()` (5xx, 408, 429, timeouts, and retryable io/hyper errors, vendored from reqwest-middleware in `retry.rs`).

### Per-host rate limiting (ratelimit module)

Yes, per-host rate limiting exists in the lib since the `ratelimit` module landed (present in 0.24.x).
Architecture: `HostPool` (DashMap of `HostKey -> Arc<Host>`, hosts created lazily) routes every request; each `Host` owns:
- a `governor` token-bucket `RateLimiter` (direct, burst 1) driven by `request_interval` (default 50ms between requests to the same host),
- a `tokio::sync::Semaphore` for per-host concurrency (default 10 concurrent requests per host),
- an adaptive backoff `Duration`: exponential on 429 (500ms doubling, capped 30s), additive on 5xx (+200ms, capped 10s), reset on 2xx,
- Retry-After / rate-limit header parsing via the `rate-limits` crate (`RateLimit::new(headers)`), which raises the backoff to the server-requested reset time, capped at `MAXIMUM_BACKOFF` = 60s,
- an in-memory per-host response cache keyed by `(Method, Uri)` (fragment stripped), plus an `active_requests` mutex map that deduplicates identical concurrent requests,
- `HostStats` tracking (success rate, cache hit rate, response times).

`RateLimitConfig { concurrency, request_interval }` sets global per-host defaults; `HostConfigs` (a `HashMap<HostKey, HostConfig>`) allows per-host overrides of concurrency, interval, and extra headers, and is TOML-(de)serializable.
So per-host concurrency, per-host pacing, Retry-After respect, exponential backoff on 429/5xx, and in-run URL dedupe are all inside the library, not the bin.

### Cache story

There are two caches and only one is in the lib:
- In-run cache: per-host `DashMap<(Method, Uri), CacheableResponse>` inside `Host` (lib). Never persisted, never TTL-expired; it exists to dedupe within a single invocation. Responses with retryable statuses are not cached.
- Persistent `--cache`: implemented entirely in lychee-bin (`lychee-bin/src/cache.rs`). A `DashMap<Uri, CacheValue { CacheStatus, timestamp }>` serialized to CSV (`.lycheecache`), loaded with `max_age_secs` TTL filtering, errors never persisted.
The lib only exposes the `CacheStatus` enum (a simple serializable representation of a cached outcome) to support such external caches.
Conclusion: a TTL response cache must be built by the consumer; lychee-lib gives you the `CacheStatus` vocabulary but no persistence, TTL, or storage layer.

### Global concurrency

lychee-lib has no global (cross-host) concurrency limit.
The lychee bin drives checks through `par-stream`/`futures` buffering with `--max-concurrency` (default 128) at the stream level, outside the client.
A consumer must apply its own global limiter (e.g. `futures::stream::buffer_unordered(n)` or a global `tokio::sync::Semaphore`) around `client.check()` calls.

## stalelink policy needs vs what lychee-lib provides

| stalelink polite-by-default need | lychee-lib 0.24.2 | Verdict |
| --- | --- | --- |
| reqwest client config (UA, timeout, TLS, redirects, headers) | `ClientBuilder` covers all of it | Reuse |
| HEAD-to-GET fallback | `method(Methods)` ordered method list with fallback loop | Reuse (configure `[HEAD, GET]`; default is GET only) |
| Retry with exponential backoff | `max_retries` + `retry_wait_time` doubling, retry classification vendored from reqwest-middleware | Reuse |
| Retry-After respect | `Host::parse_rate_limit_headers` via `rate-limits` crate, capped at 60s | Reuse |
| Per-host concurrency ~4 | Per-host semaphore, default 10, configurable via `RateLimitConfig { concurrency: 4, .. }` | Reuse (set to 4) |
| Per-host pacing | governor token bucket, default 50ms interval, configurable | Reuse |
| Adaptive backoff on 429/5xx | Built into `Host` (exp on 429 capped 30s, additive on 5xx capped 10s) | Reuse |
| Global concurrency ~128 | Not in the lib; lychee-bin does it at the stream layer | Build (trivial: buffer_unordered or global semaphore) |
| URL dedupe within a run | Per-host `(Method, Uri)` cache + active-request mutex map | Reuse (plus dedupe the input set before dispatch, which is cheap) |
| TTL response cache across runs | Only in lychee-bin (CSV + timestamp + max_age); lib exposes `CacheStatus` only | Build |
| Status classification (dead vs outdated vs transient) | `Status` / `ErrorKind` / `StatusCodeSelector` / `CacheStatus` | Reuse types, map to stalelink verdicts |

## License, MSRV, feature flags

- License: `Apache-2.0 OR MIT` (workspace Cargo.toml and docs.rs both confirm). MIT-compatible; no obstacle for stalelink.
- MSRV: `rust-version = "1.88.0"`, edition 2024. stalelink inherits this floor if it depends on lychee-lib.
- Feature flags: `default = ["email-check"]`, `email-check -> mailify-lib`, and `check_example_domains` (test helper). Use `default-features = false` to drop the mail checker; note there is no flag to drop octocrab (GitHub client), html5ever/html5gum (extractors), or pulldown-cmark - the dependency tree is heavy and mostly non-optional.
- Dependency weight is the main cost of reuse: reqwest, hyper, octocrab, two HTML parsers, a Markdown parser, ring, governor, dashmap all come along even if stalelink only wants the HTTP checker.

## Reuse-vs-build boundary (the deliverable)

Take from lychee-lib (via `ClientBuilder` configuration, `default-features = false`):
1. The whole tier-1 HTTP request path: reqwest client construction, TLS, redirects, UA, custom headers, timeout.
2. HEAD-to-GET fallback: `.method(Methods::try_from(vec![Method::HEAD, Method::GET])?)`.
3. Retry/backoff: `.max_retries(n)` + `.retry_wait_time(d)`; classification logic (retry.rs) comes free.
4. Per-host throttling: `.rate_limit_config(RateLimitConfig { concurrency: 4, request_interval })` and optional `.hosts(HostConfigs)` per-host overrides.
5. Retry-After and rate-limit header respect, adaptive 429/5xx backoff: automatic inside `Host`.
6. In-run request dedupe and per-host response memoization: automatic inside `Host`.
7. Types: `Status`, `ErrorKind`, `CacheStatus`, `StatusCodeSelector` as the vocabulary for stalelink's own verdict mapping.

Build in stalelink:
1. Global concurrency (~128): wrap `client.check()` calls in `futures::StreamExt::buffer_unordered(128)` or a global `tokio::sync::Semaphore`. lychee-bin does exactly this outside the lib; it is a few lines.
2. TTL response cache across runs: lychee's `--cache` lives in the bin (CSV of `Uri -> (CacheStatus, timestamp)` with max-age filtering, errors never persisted). stalelink must implement its own persistent cache; reusing the `CacheStatus` enum keeps the semantics aligned. Errors-never-cached and TTL-on-load are the two policies worth copying.
3. Input-set URL dedupe: collapse duplicate URLs across documents before dispatch (a `HashSet<Url>`); the lib only dedupes at the per-host request layer.
4. Verdict mapping and reporting: translate `Status`/`ErrorKind` into stalelink's dead/outdated/transient taxonomy.
5. Everything non-HTTP (document scanning, link extraction from local files) - stalelink should not use lychee's `Collector`/extractors if it has its own scanner, and skipping them avoids nothing dependency-wise anyway since they are not feature-gated.

Divergences to encode in stalelink defaults:
- Per-host concurrency: lychee defaults to 10; stalelink wants 4 - set `RateLimitConfig.concurrency = 4`.
- Method order: lychee defaults to GET-only; stalelink wants HEAD-first - configure explicitly.
- Global concurrency 128 and the TTL cache are stalelink-side responsibilities by construction.

## Gap crates (only if building instead of reusing)

If the dependency weight of lychee-lib (octocrab, html5ever, pulldown-cmark, ring, all non-optional) is unacceptable, the build-it-yourself stack is:
- `governor` 0.10 for per-host token-bucket rate limiting (exactly what lychee uses internally).
- `tokio::sync::Semaphore` for global and per-host concurrency caps (std tokio, no extra crate).
- `rate-limits` 0.7 (github.com/mre/rate-limits, same author as lychee) for parsing Retry-After and vendor rate-limit headers.
- `dashmap` 6 for the concurrent host map and caches.
- `reqwest-middleware` + `reqwest-retry` as an alternative retry layer, or vendor ~160 lines like lychee's retry.rs does.
- For the persistent TTL cache: `serde` + CSV or JSON lines keyed by URL with a timestamp, mirroring lychee-bin's cache.rs design.

Recommendation: depend on lychee-lib with `default-features = false` for the entire tier-1 HTTP checker, accept the MSRV 1.88 floor and the heavy dependency tree, and build only the global concurrency wrapper, the persistent TTL cache, input dedupe, and verdict mapping.
If binary size or compile time later becomes a hard constraint, the gap-crate stack above replicates the checker with ~500 lines of code, using the same governor/rate-limits crates lychee itself uses.
