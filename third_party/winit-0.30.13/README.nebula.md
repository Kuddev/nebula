# Nebula winit 0.30.13 overlay

This directory is the crates.io `winit` 0.30.13 source with one targeted
Windows backend backport from upstream commit `488c036a05d418e13bbdcdc349e2db2f6b7f58e2`:

- Windows 10 builds below 22000 keep the legacy mixed-DPI reposition workaround.
- Windows 11 builds use the `WM_DPICHANGED` suggested rectangle directly.

The upstream commit is on a newer winit API generation whose `EventLoop` is no
longer generic. Keeping this 0.30.13 overlay preserves Nebula's existing
`EventLoop<Event>` and `rwh_06` integration. Remove the overlay once an
API-compatible crates.io release contains the same fix.
