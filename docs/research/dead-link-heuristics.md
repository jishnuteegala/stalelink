# Dead-link heuristics: soft-404, login-wall, and staleness detection

Date: 2026-08-04
Question: what heuristics do existing tools and the literature use to detect soft-404s, login walls, and stale content, and which are worth implementing in stalelink?
Issue: https://github.com/jishnuteegala/stalelink/issues/5

Sources are primary: tool source code on GitHub, the Bar-Yossef et al. paper (WWW 2004), and archive.org API docs.

## Prior art

### lychee (github.com/lycheeverse/lychee)

lychee is purely status-code driven.
A link is OK if the final status code is in the accepted set, which defaults to 100..=103 and 200..=299 (`lychee-lib/src/types/status_code_selector.rs`, `default_accepted`).
It tries methods in order (HEAD then GET fallback) because servers answer HEAD with 404/403/405 or connection resets (`lychee-lib/src/checker/website.rs`).
It retries with exponential backoff, and tracks the full redirect history for reporting.
It has no soft-404, login-wall, or staleness detection at all.
What it has instead is a per-site "quirks" table (`lychee-lib/src/quirks/mod.rs`): rewrite YouTube video URLs to a thumbnail probe on img.youtube.com, add an Accept header for crates.io, rewrite GitHub blob markdown links to raw.githubusercontent.com, and fall back to the GitHub API when a github.com URL fails (rate limiting produces false 404s otherwise).
The GitHub API fallback treats a reachable private repo as OK, an explicit precedent for "authenticated resource exists, do not report dead".
Lesson for stalelink: the quirks-table pattern (per-host URL rewrites to cheap canonical probes) is the practical way to fight both soft-404s and login walls on big known hosts.

### linkchecker (github.com/linkchecker/linkchecker)

linkchecker always uses GET and follows redirects manually, emitting a warning (`WARN_HTTP_REDIRECTED`) for every hop with old URL, status, and reason (`linkcheck/checker/httpurl.py`).
Status >= 400 is invalid, except 429 which becomes a "rate limited" warning carrying Retry-After.
204 No Content produces an empty-content warning rather than a failure.
Its `RegexCheck` content plugin (`linkcheck/plugins/regexcheck.py`) is the closest thing to soft-404 detection in a mainstream checker: a user-supplied regex ("This page has moved|Oracle Application error") is run against the body of pages that returned 2xx, and matches become warnings with a line number.
The docstring explicitly frames it as detecting "pages that contain some form of error message".
Lesson: body-phrase matching demoted to a warning (not a failure) is the established false-positive-safe posture.

### W3C link checker (github.com/w3c/link-checker)

checklink maps ambiguous status codes to distinct internal causes: 403 with "Forbidden by robots.txt" becomes RC_ROBOTS_TXT, 403 for non-public IPs becomes RC_IP_DISALLOWED, 500 with a bad-hostname message becomes RC_DNS_ERROR (`bin/checklink`, `record_results`).
On 401 it retries with Basic credentials if configured, tracks the realm, and offers `--hide-same-realm` to suppress 401s within a realm the user already authenticated to.
It records the original status code behind redirect chains and lets users suppress specific redirects and specific broken codes per URL.
Lesson: do not treat 401/403 as dead; treat them as a distinct category, and disambiguate robots-blocked 403s from real ones.

### broken-link-checker (github.com/stevenvachon/broken-link-checker)

Reports every failure as a machine-readable reason code (HTTP_404, ERRNO_ECONNRESET, BLC_ROBOTS) rather than a boolean.
Honors `<meta name="robots">`, X-Robots-Tag, and the `unavailable_after` directive, which is itself a staleness signal published by the page.

### Bar-Yossef et al., "Sic transit gloria telae" (WWW 2004)

The classic soft-404 paper; found that soft-404s account for more than 25% of dead links in their crawl.
Their `isDeadPage(u)` algorithm (Table 1 of the paper) is the canonical random-URL fingerprint technique:
1. Fetch u, following up to 20 redirects; 403/404/410/5xx, timeouts, and redirect loops are hard failures (dead).
2. Construct r = u.parent + 25 random letters, i.e. a sibling URL in the same directory, guaranteed nonexistent with probability ~1 - N/26^25.
3. Fetch r the same way. If r errors, the server produces honest 404s in this directory, so u is alive.
4. If u is the root of its host, declare alive (a root cannot be a soft-404; this is also the known weakness for parked domains).
5. If the redirect counts differ (Ku != Kr), declare alive, even when both land on the same final URL.
6. If final URLs match and redirect counts match, u is a soft-404.
7. If final URLs differ but redirect counts match and the contents Tu and Tr are identical or near-identical (shingling), u is a soft-404.
Key design points worth copying:
The probe must be in the same directory as u, not at the host root, because large hosts route directories to different servers (their example: www.ibm.com/blablabla returns 404 while www.ibm.com/us/blablabla soft-redirects).
Redirect-count comparison catches the eurosport.de case where both real and fake pages end at the same URL but via different chain lengths.
Near-identity (not equality) matters because servers embed the requested URL in their error page.
Known failure mode: parked or repurposed domains where the root itself is effectively a soft-404.

### Successors: content classifiers

Meneses, Furuta, Shipman, "Identifying 'Soft 404' Error Pages" (TPDL 2012, cs.odu.edu/~sampath/publications/conferences/jcdl212-meneses.pdf cites it) classify soft-404s from the page text alone using lexical signatures, avoiding the extra probe request.
Follow-up work by the same group on ACM DL conference links found decay progresses through stages (kind-of-correct, redirects, directory listings, error pages, domain takeover), with domain takeover the hardest case, matching Bar-Yossef's parked-domain caveat.
Open-source implementations exist: TeamHG-Memex/soft404 (Python, trained classifier, `soft404.probability('<h1>Page not found</h1>')` -> 0.97) and its retrained fork dogancanbakir/soft-404.
Lesson: a phrase or lexical heuristic on the body is a zero-extra-request approximation; the probe technique is the confirmation step.

### archive.org Wayback APIs (archive.org/help/wayback_api.php)

Availability API: `https://archive.org/wayback/available?url=<url>` returns the closest snapshot with its own archival status code, or `{"archived_snapshots":{}}`.
Optional `timestamp=YYYYMMDDhhmmss` returns the snapshot closest to that time, which allows asking "was this page alive at time T" and comparing snapshot ages.
The CDX server API allows richer queries (all captures, status codes over time) for deeper staleness analysis.
Uses: (a) suggest replacement URLs for dead links; (b) staleness signal - if the live page differs radically from all snapshots or the last 200 capture is years old; (c) corroborate death - a URL whose recent captures are all 404/redirect in the CDX data.

## Signal family (a): soft-404

| Signal | Mechanism | Extra requests | False-positive risk |
| --- | --- | --- | --- |
| Error-phrase match in title/body | After a 200, scan `<title>` and main text for phrases: "page not found", "404", "no longer available", "this page doesn't exist", "content has moved", "has been removed". linkchecker RegexCheck precedent. | 0 (body already fetched on GET) | Medium. Docs pages about 404 handling, blog posts quoting the phrases. Weight title matches higher than body matches; never a hard verdict alone. |
| Random-sibling probe (Bar-Yossef) | Fetch u.parent + ~25 random chars once per (host, directory), cache the result. If the probe returns 200, the server soft-404s; compare u against probe. | 1 per unique (host, parent-dir), amortized across links | Low when combined with the comparisons below. Fails on parked domains and on roots (skip roots). Cost concern: doubles requests on sites with one link per directory. |
| Same final URL + same redirect count as probe | After probe: wu == wr and Ku == Kr means u indistinguishable from a nonexistent page. | 0 beyond the probe | Very low. The strongest single soft-404 confirmation. |
| Near-identical content vs probe | wu != wr but Ku == Kr and shingle/simhash(Tu) ~= shingle(Tr). Catches per-URL error pages (amazon.com style). | 0 beyond the probe (needs GET bodies) | Low. Requires a similarity threshold; template-heavy sites with tiny content areas can collide. |
| Content-length ratio vs probe | Cheap pre-filter for the above: if |len(Tu) - len(Tr)| / max tiny, escalate to shingling. | 0 | Medium alone; use only as a trigger for the similarity check. |
| Redirect-to-homepage | Final URL after redirects is the host root (or bare path "/") while the original URL had a deeper path. | 0 | Medium. Some sites legitimately redirect moved content to a landing page; sites redirect mobile/geo variants to the root. Demote to suspect, not dead. |
| Tiny-body 200 | 200 with Content-Length 0 or near-0 on a text/html URL (linkchecker warns on 204 similarly). | 0 | Medium. SPAs render from JS with near-empty HTML shells. Suspect only. |

## Signal family (b): login-wall

| Signal | Mechanism | Extra requests | False-positive risk |
| --- | --- | --- | --- |
| 401 status | HTTP semantics: authentication required. W3C checker treats it as its own category with realm tracking. | 0 | Very low for "auth-walled". WWW-Authenticate header confirms. |
| 403 status | Ambiguous: auth-gated, IP-blocked, robots-blocked, or bot-detection (Cloudflare). W3C checker splits 403 by response message. | 0 | Medium as auth signal. Classify as auth-walled only with corroboration (auth-path redirect, WWW-Authenticate); otherwise suspect. Bot-detection 403s are the big trap for CLI checkers. |
| Redirect chain lands on auth URL pattern | Final (or any) hop matches /login, /signin, /sign-in, /auth, /sso, /oauth, /account/login, accounts.google.com, login.microsoftonline.com, /idp/, /saml, ?returnUrl=, ?redirect_uri=, ?next= pointing back at the original path. The returnUrl-back-to-original parameter is the strongest form. | 0 (redirect history already tracked, as in lychee) | Low with a curated pattern list plus return-URL check. A page legitimately named /login would match; rare in document links. |
| Meta-refresh or JS redirect to login | 200 body contains `<meta http-equiv="refresh" content="...url=<auth-url>">` or a window.location assignment to an auth pattern. | 0 (body scan) | Low for meta-refresh; JS-detection is regex-fragile, keep to well-known SSO hosts. |
| Cookie-gated 200 | Page returns 200 but content is a login form: `<form>` with password input, or phrases "sign in to continue", "log in to view". stalelink's browser-cookie tier exists exactly for this: recheck with profile cookies and compare. | 0 to detect; 1 recheck through the authenticated tier to confirm | Medium on detect (pages containing login forms in headers); low after the authenticated recheck differs. |
| Known-SSO host allowlist | Final host in a maintained list (accounts.google.com, login.live.com, github.com/login, id.atlassian.com, okta domains, auth0 domains). | 0 | Very low. Highest-precision login-wall signal. |

## Signal family (c): staleness

| Signal | Mechanism | Extra requests | False-positive risk |
| --- | --- | --- | --- |
| Deprecation/archived banner phrases | Body/title scan for "deprecated", "this documentation is archived", "no longer maintained", "end of life", "this version is out of date", "you are viewing documentation for an older version". Common in docs frameworks (Sphinx, Docusaurus, MkDocs emit standard banners). | 0 | Low-medium. Pages discussing deprecation of something else. Restrict to banner-like positions (first N bytes, elements with class names like "admonition", "deprecated", "version-banner") to cut noise. |
| Version-segment drift | URL contains /v1/, /1.x/, /en/1.0/ style segments. Probe: rewrite the segment to the next plausible version (/v2/, /latest/, /stable/) and HEAD it; if the higher version exists and the linked one shows a banner or redirects, flag outdated. Also probe the segment replaced with "latest". | 1-2 HEAD probes per versioned URL, cache per (host, family) | Medium. /v2/ existing does not make /v1/ wrong (APIs keep versions live deliberately). Only flag as outdated, never dead; suppress when the linked version is clearly intentional (pinned docs). |
| Far-past Last-Modified | Last-Modified (or Age-derived) header older than a threshold (e.g. 5+ years) on a text/html page. | 0 | High. Many stable, correct pages (RFCs, specs, papers) are old by design. Use only as a corroborating signal or behind an opt-in flag; never alone. |
| meta robots unavailable_after | `unavailable_after` in X-Robots-Tag or meta robots is the publisher itself declaring an expiry (broken-link-checker parses it). | 0 | Very low, but rare in the wild. |
| Wayback availability/CDX | Availability API: 1 request per URL to check archive presence and last-capture age; CDX for capture history. If live page is dead, a snapshot enables a replacement suggestion; if last successful capture is ancient and recent captures are errors, corroborates decay. | 1 per URL (batchable, rate-limited third-party dependency; should be opt-in or dead-links-only) | Low for replacement suggestions. As a staleness signal, medium: crawl frequency reflects popularity, not correctness. |
| Redirect to an "archive" path | Final URL contains /archive/, /legacy/, web.archive.org, or a "-archived" slug when the original did not. | 0 | Low. Publisher self-declared archival. |

## Mapping to stalelink confidence levels

stalelink levels: dead-certain, likely-dead, auth-walled, outdated, suspect.

dead-certain:
- Hard failures: DNS NXDOMAIN, connection refused, TLS hostname mismatch on a domain with no fallback, HTTP 404/410 on GET (after HEAD->GET fallback, as lychee does).
- Soft-404 confirmed by the Bar-Yossef probe: probe returns 200 and (same final URL + same redirect count) or (same redirect count + near-identical body).
- 404/410 corroborated by Wayback CDX showing recent captures also failing (when Wayback checking is enabled).

likely-dead:
- Strong error-phrase match in `<title>` ("404", "page not found") on a 200 without probe confirmation.
- Redirect-to-homepage from a deep path combined with a body error phrase or with near-zero content.
- 5xx that persists across retries (transient 5xx and 429 should stay suspect, per linkchecker's 429 handling).
- Parked-domain indicators (registrar lander phrases, domain-for-sale content) where probe logic cannot apply.

auth-walled:
- 401 always.
- Redirect chain landing on an auth URL pattern or known SSO host, especially with a return-URL parameter pointing back at the original path.
- Meta-refresh to an auth URL.
- 200 whose body is a login form, confirmed when the browser-cookie or CDP tier gets different content (stalelink's tiered design: never report auth-walled links as dead).
- 403 only with corroboration (WWW-Authenticate, auth-path redirect); an uncorroborated 403 is suspect because of bot-detection walls.

outdated:
- Deprecation/archived banner phrase in banner position on a 200.
- Version-segment drift where a newer version probe succeeds and the linked page self-identifies as old or redirects.
- Redirect to an /archive/ or /legacy/ path.
- unavailable_after directive in the past.
- Far-past Last-Modified only as a corroborating signal combined with any of the above, or behind an opt-in threshold flag.

suspect:
- Body error phrase without title match and without probe confirmation.
- Redirect-to-homepage alone.
- Tiny-body 200 alone.
- Uncorroborated 403, persistent 429, transient 5xx and timeouts.
- Content-length anomaly vs directory probe that did not pass the similarity check.
- Old Last-Modified alone; Wayback last-capture-age alone.

## Recommended implementation order

1. Zero-cost signals first: status semantics (401/403/410 split), redirect-chain auth patterns, redirect-to-homepage, title/body phrase lists, meta-refresh parsing. These need no extra requests and reuse the response already in hand.
2. The Bar-Yossef directory probe, cached per (host, directory), triggered only when a zero-cost signal fires or when a 200 follows a redirect. This bounds extra requests while giving the only high-confidence soft-404 verdict available.
3. Version-drift probing and Wayback integration as opt-in features, since both add third-party or speculative requests, and Wayback doubles as the replacement-URL suggester stalelink already promises.
4. A trained content classifier (soft404-style lexical model) is not worth the dependency for a CLI; the phrase list plus probe covers the same ground with explainable output.
