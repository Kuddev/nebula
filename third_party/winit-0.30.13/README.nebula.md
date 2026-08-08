# Nebula winit 0.30.13 overlay

This directory is the crates.io `winit` 0.30.13 source with one targeted
Windows backend backport from upstream commit `488c036a05d418e13bbdcdc349e2db2f6b7f58e2`:

- Windows 10 builds below 22000 keep the legacy mixed-DPI reposition workaround.
- Windows 11 builds use the `WM_DPICHANGED` suggested rectangle directly.
- Windows key events retain the raw `WM_KEYDOWN/UP` virtual key, scan code,
  repeat count, extended flag, layout-resolved UTF-16 character, and
  `KEY_EVENT_RECORD` control-key state. The
  public `winit::platform::windows::KeyEventExtWindows` trait exposes that
  snapshot to the terminal input adapter without a second Win32 message hook
  or a `MapVirtualKeyW` reverse lookup.

The upstream commit is on a newer winit API generation whose `EventLoop` is no
longer generic. Keeping this 0.30.13 overlay preserves Nebula's existing
`EventLoop<Event>` and `rwh_06` integration. Remove the overlay once an
API-compatible crates.io release contains the same fix.

When updating this overlay, preserve the Windows `KeyEventExtra::raw` field and
the `PartialKeyEventInfo`/`KeyLParam` propagation. The terminal's Win32 input
mode depends on those fields for layout-independent functional keys and
modifier chords; printable and IME text remains on Winit's normal text path.
