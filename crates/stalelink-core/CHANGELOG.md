# Changelog

All notable changes to this project will be documented in this file.

## [0.1.1] - 2026-11-13

### Features

- Accept TOML 1.1 syntax in stalelink.toml


### Bug Fixes

- Align user-facing surfaces and release docs with shipped state ([#78](https://github.com/jishnuteegala/stalelink/pull/78))

- Upgrade zip, lopdf, rustls and wrangler to clear security alerts ([#89](https://github.com/jishnuteegala/stalelink/pull/89))

- Refuse PDFs that open with an empty user password

- Move off yanked chacha20 0.10.1 and async-executor 1.14.0

- Decode character references and predefined entities in field instructions


### Performance

- Share one soft-404 sibling probe per directory ([#101](https://github.com/jishnuteegala/stalelink/pull/101))

- Reuse the GET fallback walk in heuristics

- Buffer report writes


### Build System

- Upgrade quick-xml to 0.42 and toml to 1.2 ([#90](https://github.com/jishnuteegala/stalelink/pull/90))


## [0.1.0] - 2026-08-07

### Features

- Scaffold workspace, core model, CLI skeleton, and CI ([#26](https://github.com/jishnuteegala/stalelink/pull/26))

- Scan tracer bullet — end-to-end link scan for text formats ([#27](https://github.com/jishnuteegala/stalelink/pull/27))

- Verdict intelligence — soft-404, login-wall, redirects, staleness, version drift ([#28](https://github.com/jishnuteegala/stalelink/pull/28))

- PDF + OOXML extractors, format auto-detection ([#29](https://github.com/jishnuteegala/stalelink/pull/29))

- Local link validation, anchors, and mailto/tel syntax checks ([#30](https://github.com/jishnuteegala/stalelink/pull/30))

- Stalelink fix for text formats - diff, --write, verify + restore ([#33](https://github.com/jishnuteegala/stalelink/pull/33))

- Stalelink fix for binary formats - PDF incremental + OOXML raw-copy rewrite ([#34](https://github.com/jishnuteegala/stalelink/pull/34))

- Auth tiers 2+3 - cookie escalation, CDP, failure UX ([#35](https://github.com/jishnuteegala/stalelink/pull/35))

- Polish + release pipeline - README, completions, release-plz + cargo-dist, channels ([#36](https://github.com/jishnuteegala/stalelink/pull/36))

- Full distribution channel matrix - brew tap, scoop, winget, nfpm, AUR, npm OIDC ([#41](https://github.com/jishnuteegala/stalelink/pull/41))

