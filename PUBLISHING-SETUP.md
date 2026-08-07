# Publishing operations

This guide records the external account configuration and credential maintenance
that cannot be inferred from the repository. It contains no secret values and is
safe to keep under version control.

## Current channels

| Channel | Credential | External state |
| --- | --- | --- |
| GitHub Releases | `GITHUB_TOKEN` | Workflow is ready; the first release is pending. |
| npm | Trusted publishing (OIDC), no stored secret | Not bootstrapped; complete [npm trusted publishing](#npm-trusted-publishing) before the first release. |
| Homebrew and Scoop | `PACKAGES_GITHUB_TOKEN` | `jishnuteegala/homebrew-tap` and `jishnuteegala/scoop-bucket` exist; no stalelink manifest is published yet. Complete the [credential setup](#channel-credentials). |
| WinGet | `WINGET_GITHUB_TOKEN` | The `jishnuteegala/winget-pkgs` fork exists; no stalelink manifest is submitted yet. Complete the [credential setup](#channel-credentials). |
| AUR | `AUR_KEY` | `stalelink-bin` is not published; complete the [AUR setup](#aur) and add its secret. |
| Chocolatey | `CHOCOLATEY_API_KEY` | No package has been submitted; complete the [Chocolatey setup](#chocolatey) and add its secret. |
| crates.io | `CARGO_REGISTRY_TOKEN` | Not published; add the token during [credential setup](#setting-secrets) before the first release. |

## npm trusted publishing

npm trusted publishing exchanges GitHub's short-lived OIDC identity for a
single-use publish credential. It requires Node 22.14 or newer, npm 11.5.1 or
newer, a GitHub-hosted runner, and `id-token: write`. The publish job already
uses Node 24, npm 12, and the required permission.

Bootstrap the sole cargo-dist npm package before merging the first release PR:

```sh
gh api repos/jishnuteegala/.github/contents/scripts/npm-oidc-bootstrap.sh --jq .content | base64 -d > npm-oidc-bootstrap.sh
bash npm-oidc-bootstrap.sh all --repo jishnuteegala/stalelink --workflow release.yml @jishnuteegala/stalelink
```

The package is the scoped `@jishnuteegala/stalelink`: npm rejects the unscoped
name as too similar to the existing `stylelint` package (E403).

The central script publishes a `0.0.0` placeholder under the `bootstrap` tag,
configures trusted publishing, locks publishing to 2FA, and verifies the result.
Its `publish` and `lock` phases prompt for web-based EOTP, so run them
interactively. An agent may run its `trust` and `verify` phases without an OTP.

Configure one npm trusted publisher:

| Field | Value |
| --- | --- |
| Organization or user | `jishnuteegala` |
| Repository | `stalelink` |
| Workflow filename | `release.yml` |
| Environment | Leave empty |
| Allowed actions | `npm publish` |

Use `release.yml`, not `publish-npm-oidc.yml`. npm validates the calling
workflow when a reusable workflow performs publishing. `release-plz.yml`
dispatches the top-level `release.yml`; that workflow calls the reusable
`publish-npm-oidc.yml`, so the OIDC claim carries `release.yml` as the calling
workflow. `publish-npm-oidc.yml` cannot be dispatched independently.

The initial bootstrap happens before the first release. Merge the release PR
after it completes; the real `0.1.0` release then publishes with OIDC and npm
provenance. The workflow tolerates npm E404 only as a recovery fallback: it
emits a prominent warning and allows the release to become public without an
npm package. Bootstrap before the first release rather than relying on this
fallback.

Set publishing access to require 2FA and disallow tokens; OIDC trusted
publishing is unaffected:

```sh
npm access set mfa=publish @jishnuteegala/stalelink
```

After every release, verify npm provenance:

```sh
version=0.1.0
npm view "@jishnuteegala/stalelink@$version" --json dist.attestations \
  | node -e 'const a=JSON.parse(require("fs").readFileSync(0,"utf8"));process.exit(a&&a.provenance?0:1)' \
  && echo 'stalelink: provenance OK' || echo 'stalelink: NO PROVENANCE'
```

The package page should show npm's "Built and signed on GitHub Actions"
provenance badge linking to this repository.

## Channel credentials

### Homebrew and Scoop

`PACKAGES_GITHUB_TOKEN` is one fine-grained GitHub PAT with access only to
`jishnuteegala/homebrew-tap` and `jishnuteegala/scoop-bucket`. Its repository
permission is **Contents: Read and write**; no account permissions are needed.

### WinGet

`WINGET_GITHUB_TOKEN` writes the version branch to the
`jishnuteegala/winget-pkgs` fork and opens or discovers the upstream Microsoft
PR. Use a fine-grained PAT scoped to the fork with **Contents: Read and write**.
If GitHub rejects upstream PR operations with that token, use a classic token
limited to `public_repo`.

### AUR

`AUR_KEY` is an unencrypted, dedicated Ed25519 private key. Its public key must
remain registered in the AUR account. Keep the keypair outside repositories:

```sh
ssh-keygen -t ed25519 -f ~/.ssh/aur_stalelink_ed25519 -N ""
gh secret set AUR_KEY --repo jishnuteegala/stalelink < ~/.ssh/aur_stalelink_ed25519
```

### Chocolatey

`CHOCOLATEY_API_KEY` comes from the Chocolatey account page. The publisher
submits a package whose installer pins the released Windows ZIP URLs to their
SHA256 checksums. A successful push means submitted, not necessarily publicly
installable: validation, scanning, and moderation follow.

### crates.io

`CARGO_REGISTRY_TOKEN` is a crates.io API token with publish authority for
`stalelink-core` and `stalelink`. Create it at
<https://crates.io/settings/tokens/new> with exactly these values:

| Field | Value |
| --- | --- |
| Name | `stalelink-release` |
| Expiration | 90 days (add a rotation reminder; see [rotation](#credential-rotation-and-incident-response)) |
| Scopes | `publish-new` and `publish-update` only (the first release publishes both crates as new; later releases are updates) |
| Crates | The pattern `stalelink*`, not Unrestricted (matches `stalelink-core` and `stalelink`, including before first publication) |

Do not grant `change-owners`, `yank`, or `trusted-publishing`.

## Setting secrets

For single-line credentials, let `gh` prompt so values do not enter shell
history:

```sh
gh secret set PACKAGES_GITHUB_TOKEN --repo jishnuteegala/stalelink
gh secret set WINGET_GITHUB_TOKEN --repo jishnuteegala/stalelink
gh secret set CHOCOLATEY_API_KEY --repo jishnuteegala/stalelink
gh secret set CARGO_REGISTRY_TOKEN --repo jishnuteegala/stalelink
```

List secret names and update times with:

```sh
gh secret list --repo jishnuteegala/stalelink
```

GitHub never reveals stored secret values. A recent update time proves only
that a value was stored, not that it is valid.

## Monitoring

After every release:

1. Confirm the **Release** workflow completed.
2. Confirm GitHub release assets match `sha256.sum`.
3. Confirm `@jishnuteegala/stalelink` exists on npm and its provenance is linked to this repository.
4. Confirm the Homebrew formula and Scoop manifest reference the released version.
5. Confirm the WinGet PR is open or merged and its validation checks pass.
6. Confirm the AUR package version, source checksum, and `git` dependency.
7. Confirm Chocolatey is approved and its verifier/scan results pass.
8. Confirm `stalelink-core` and `stalelink` appear at the expected crates.io versions.

Use **Actions -> Release -> Run workflow** with an existing immutable `v*` tag
to retry the cargo-dist release path. It must never create a new version.

## Rotation and incident response

Review credentials quarterly and after maintainer, repository, or account
changes. Also monitor provider expiry emails and failed publishing jobs.

| Credential | Normal maintenance | Rotation procedure |
| --- | --- | --- |
| npm trusted publisher | Audit the package configuration quarterly | No secret rotation; update the publisher immediately if repository/workflow identity changes. If OIDC breaks, temporarily allow a short-lived package token only to recover, then restore disallow-tokens and revoke it. |
| `PACKAGES_GITHUB_TOKEN` | Check expiry, selected repositories, and Contents permission quarterly | Create replacement, update secret, prove a write on the next new manifest publication, then revoke old PAT. |
| `WINGET_GITHUB_TOKEN` | Check expiry, fork scope, and Contents permission quarterly | Create replacement, update secret, prove a write on the next new WinGet branch, then revoke old PAT. |
| `AUR_KEY` | Check registered AUR public keys quarterly | Generate a new pair, register its public key, update secret, prove a package update, then remove the old key. |
| `CHOCOLATEY_API_KEY` | Check Chocolatey account and moderation notifications quarterly | Regenerate on Chocolatey, immediately update the GitHub secret, and prove it on the next new submission. |
| `CARGO_REGISTRY_TOKEN` | Check crates.io token ownership and publish scope quarterly | Create replacement, update secret, prove it on the next ordered crates.io publication, then revoke old token. |

If a credential may be exposed, revoke or remove it at the provider first,
rotate it, inspect workflow and provider audit logs, and rerun only after the
new credential is installed. Deleting a GitHub secret alone does not revoke the
credential at its provider.

## Repository controls

- Keep default workflow permissions read-only.
- Prevent Actions from approving pull requests.
- Require maintainer approval before workflows from external forks run.
- Require CI and release checks on `main`.
- Protect immutable `v*` tags.
- Allow only GitHub-owned actions and explicitly approved third-party actions,
  enforcing full-SHA pinning in repository settings.
- Keep PR CI on `pull_request` with read-only permissions and no secrets.
  Never check out fork code from `pull_request_target` or a privileged
  `workflow_run`.
- Keep CodeQL default setup enabled for Rust, JavaScript, and Actions.
- Keep publishing credentials in GitHub Actions secrets, never in this file.
