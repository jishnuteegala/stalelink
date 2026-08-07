# Release Setup

The first npm publication uses a two-phase draft release because npm trusted publishing can only be configured after the package exists.

1. Merge the release-plz PR. It creates a tag and dispatches the release workflow.
2. The release workflow creates a draft and uploads the cargo-dist artifacts. Its npm publisher reports a missing trusted-publisher configuration without blocking the draft from being made public.
3. While logged into npm, run `node scripts/bootstrap-npm.js vX.Y.Z draft`. This downloads the npm tarball from the authenticated draft release, verifies `stalelink@X.Y.Z`, and publishes it interactively.
4. Alternatively, check out the exact tag and run `node scripts/bootstrap-npm.js vX.Y.Z build`. It builds the global cargo-dist artifacts locally, then performs the same verification and interactive publication.
5. Configure npm trusted publishing for package `stalelink`, repository `jishnuteegala/stalelink`, and workflow `.github/workflows/release.yml`. Re-run the release workflow for the tag.

Channel credentials use these GitHub Actions secret names: `CARGO_REGISTRY_TOKEN`, `HOMEBREW_TAP_TOKEN`, `PACKAGES_GITHUB_TOKEN`, `AUR_KEY`, and `WINGET_GITHUB_TOKEN`. Never commit credential values.
