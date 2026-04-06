# Repo Release Runbook

This document is the maintainer runbook for publishing a macOS `threadBridge` release from this repo.

It reflects the current committed release contract:

- build and package with `scripts/release_threadbridge.sh`
- sign with a local `Developer ID Application` identity
- notarize with `notarytool`
- upload a GitHub prerelease with the generated DMG and checksum

## Scope

This runbook covers:

- creating release notes
- building, signing, notarizing, and uploading artifacts
- tagging the repo
- turning the GitHub draft release into a published prerelease

This runbook does not cover:

- Homebrew tap publication
- App Store distribution
- tracked `fastlane/` files

## One-Time Machine Setup

Do this once on the release machine.

### 1. Required tools

Confirm these commands are available:

```bash
cargo
rustup
codesign
xcrun
hdiutil
gh
```

If `cargo-bundle` is not installed yet, `scripts/release_threadbridge.sh` will install it automatically.

### 2. Developer ID identity

The machine must have a `Developer ID Application` identity in the login keychain.

Check:

```bash
security find-identity -v -p codesigning
```

Expected shape:

```text
Developer ID Application: Example, Inc. (TEAMID)
```

`Apple Development` is not enough for public distribution.

### 3. notarytool keychain profile

The default notarization profile name is `threadbridge-notary`.

Check:

```bash
xcrun notarytool history --keychain-profile threadbridge-notary
```

If you prefer a different profile name, pass it with `--notary-profile`.

### 4. GitHub CLI auth

The release script publishes with `gh`.

Check:

```bash
gh auth status
```

The authenticated account must be able to create releases in `qoli/threadBridge`, or in the repo passed via `--github-repo`.

## Per-Release Inputs

Every release needs:

- a version string like `0.1.0-rc.1`
- a release notes file at `docs/releases/<version>.md`
- a usable `Developer ID Application` identity string

Recommended release notes path:

```bash
docs/releases/0.1.0-rc.1.md
```

Minimal template:

```md
# threadBridge 0.1.0-rc.1

## Highlights

- Short summary of the release.

## Known Limitations

- Optional short limitation list.
```

## Standard Release Flow

### 1. Start from a clean worktree

The release script fails if the repo is dirty.

Check:

```bash
git status --short
```

Emergency escape hatch:

```bash
THREADBRIDGE_RELEASE_ALLOW_DIRTY=1
```

Do not use that for normal releases.

### 2. Run the full pipeline

Normal maintainer path:

```bash
scripts/release_rc.sh 0.1.0-rc.2
```

This wrapper:

- defaults release notes to `docs/releases/<version>.md`
- creates a release-notes stub when missing
- defaults the notary profile to `threadbridge-notary`
- bootstraps that profile from the local `fastlane/threadbridge-asc` API key when needed
- falls back to the local fastlane `bootstrap_notary_profile` lane when the ASC key path is unavailable
- defaults the GitHub repo to `qoli/threadBridge`
- auto-detects the `Developer ID Application` identity when only one is available

If you prefer the lower-level script, the equivalent explicit command is:

Example:

```bash
scripts/release_threadbridge.sh release \
  --version 0.1.0-rc.1 \
  --notes-file docs/releases/0.1.0-rc.1.md \
  --codesign-identity "Developer ID Application: Example, Inc. (TEAMID)"
```

What this does:

- builds a universal `threadBridge.app`
- signs the app with hardened runtime
- creates `threadBridge-<version>-macos-universal.dmg`
- notarizes the DMG
- staples and validates the DMG
- writes a `.sha256` file
- creates or updates a GitHub draft prerelease

Artifacts are written to:

```bash
dist/release/<version>/
```

Expected files:

- `threadBridge-<version>-macos-universal.dmg`
- `threadBridge-<version>-macos-universal.sha256`
- `threadBridge.app`

### 3. Create and push the git tag

The release script publishes the GitHub release, but it does not create the git tag for you.

If you want the wrapper to do this too, rerun with:

```bash
scripts/release_rc.sh 0.1.0-rc.2 --publish-final
```

Otherwise create the tag manually:

Create an annotated tag on the release commit:

```bash
git tag -a v0.1.0-rc.1 -m "threadBridge 0.1.0-rc.1"
git push origin v0.1.0-rc.1
```

If you want to tag a specific commit explicitly:

```bash
git tag -a v0.1.0-rc.1 <commit-sha> -m "threadBridge 0.1.0-rc.1"
git push origin v0.1.0-rc.1
```

### 4. Publish the draft prerelease

The script creates a GitHub draft prerelease. Publish it explicitly when you are satisfied with the notes and assets.

CLI path:

```bash
gh release edit v0.1.0-rc.1 --repo qoli/threadBridge --draft=false --prerelease
```

Or open it in the browser:

```bash
gh release view v0.1.0-rc.1 --repo qoli/threadBridge --web
```

### 5. Verify the final GitHub state

Check:

```bash
gh release view v0.1.0-rc.1 --repo qoli/threadBridge --json tagName,isDraft,isPrerelease,url,publishedAt,assets
```

Expected shape:

- `tagName = v0.1.0-rc.1`
- `isDraft = false`
- `isPrerelease = true`
- DMG and SHA256 assets are uploaded

## Useful Partial Commands

Use these when iterating on one stage of the pipeline:

```bash
scripts/release_threadbridge.sh build --version 0.1.0-rc.1
scripts/release_threadbridge.sh sign --version 0.1.0-rc.1 --codesign-identity "Developer ID Application: Example, Inc. (TEAMID)"
scripts/release_threadbridge.sh dmg --version 0.1.0-rc.1 --codesign-identity "Developer ID Application: Example, Inc. (TEAMID)"
scripts/release_threadbridge.sh notarize --version 0.1.0-rc.1 --codesign-identity "Developer ID Application: Example, Inc. (TEAMID)"
scripts/release_threadbridge.sh publish --version 0.1.0-rc.1 --notes-file docs/releases/0.1.0-rc.1.md
```

## Troubleshooting

### `codesign identity not found in keychain`

Check:

```bash
security find-identity -v -p codesigning
```

You need a `Developer ID Application` identity, not only `Apple Development`.

### `notarytool keychain profile is unavailable`

Check:

```bash
xcrun notarytool history --keychain-profile threadbridge-notary
```

If this fails, recreate the local notary profile before retrying.

### `working tree must be clean before publish/release`

Commit or stash your changes first.

`THREADBRIDGE_RELEASE_ALLOW_DIRTY=1` is only for local validation or emergency operator use.

### Release exists but GitHub URL looks like `untagged-*`

That usually means the git tag was not pushed yet.

Run:

```bash
git push origin v<version>
```

Then re-check:

```bash
gh release view v<version> --repo qoli/threadBridge
```
