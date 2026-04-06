# Releases

This directory contains:

- per-version release notes, for example `docs/releases/0.1.0-rc.1.md`
- the repo release runbook at [repo-release-runbook.md](/Volumes/Data/Github/threadBridge/docs/releases/repo-release-runbook.md)

Use the runbook when you need to publish a new macOS prerelease from this repo:

```bash
scripts/release_rc.sh 0.1.0-rc.2
```

Current committed contract:

- `release_threadbridge.sh` handles build, sign, DMG, notarize, and GitHub draft prerelease upload
- `release_rc.sh` is the maintainer-friendly wrapper for the normal RC path
- git tag creation and final draft publication are separate maintainer steps
- Homebrew tap publication is still out of scope for the first RC path
