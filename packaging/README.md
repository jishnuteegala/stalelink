# Packaging Automation

`nfpm.yaml` packages the cargo-dist Linux archives into deb, rpm, apk, and Arch
artifacts for x64 and ARM64 in `publish-nfpm.yml`. `aur/PKGBUILD.template` and
the Scoop template are rendered by their respective publishing workflows.

`scripts/channel-lib.sh` holds shared checksum selection and rendering logic.
`scripts/test-channels.sh` exercises it against the complete cargo-dist fixture
asset list without network access. The WinGet template documents the matching
two-architecture installer shape used by Komac.
