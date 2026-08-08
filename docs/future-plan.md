# Nebula Future Plan

## SSH update detection and upgrade flow

The Settings → SSH page now exposes a stable upgrade button, but this release
does not perform update checks or downloads. Keeping the affordance stable lets
the page layout and keyboard/mouse contract settle before introducing network
work and replacement behavior.

The future implementation should:

- Check for updates in a low-priority background task, with explicit timeout,
  cancellation, and no network work on the renderer or input thread.
- Show the upgrade action only after a signed release manifest reports a newer
  compatible version. The manifest must include the target platform, package
  size, version, and SHA-256 digest.
- Download to a sibling staging file, verify the SHA-256 before it becomes
  executable, and reject truncated, mismatched, or unexpected packages.
- On Windows, hand replacement to a small detached updater process so the
  running executable is never overwritten. Replace atomically after Nebula
  exits, then restart with the previous arguments and working directory.
- On Linux and macOS, use the platform's normal application bundle/package
  replacement rules and preserve executable permissions and extended metadata.
- Keep the previous package until the new process reports a healthy startup;
  failed launch, interrupted download, or verification error must roll back and
  leave the current installation usable.
- Record the manifest version, verification result, replacement result, and
  rollback reason in the existing log path without writing credentials or full
  URLs containing secrets.

This plan intentionally excludes update detection, download, replacement, and
rollback code from the current SSH settings migration.
