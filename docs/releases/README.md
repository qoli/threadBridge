# Release Notes

This directory stores the release notes files consumed by `scripts/release_threadbridge.sh`.

Use one markdown file per version, for example:

- `docs/releases/0.1.0-rc.1.md`
- `docs/releases/0.1.0-rc.2.md`

First RC operator flow:

```bash
scripts/release_threadbridge.sh release --version 0.1.0-rc.1 --notes-file docs/releases/0.1.0-rc.1.md --codesign-identity "Developer ID Application: Example, Inc. (TEAMID)"
```

For the first RC path, `release_threadbridge.sh` handles build/sign/dmg/notarize/publish and creates a GitHub draft prerelease. Homebrew tap publication is intentionally deferred.

If you personally prefer `fastlane` for local Apple bootstrap, keep that Fastfile private and ignored.
The committed repo contract stays shell-first:

- ensure `Developer ID Application` is visible to `codesign`
- create the local `threadbridge-notary` profile with `xcrun notarytool store-credentials`
- run `scripts/release_threadbridge.sh release`
