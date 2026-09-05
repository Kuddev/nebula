# Logo Assets

## Nebula App Icon

The rounded, asymmetric symbol and cutout prompt are shared by three curated
colorways. The product name remains Nebula.

| Setting | Source | Role |
| --- | --- | --- |
| `silver-violet` | `nebula-light.svg` | Default identity; palette 12 |
| `graphite-violet` | `nebula-dark.svg` | Graphite tile with restrained gray-violet mark |
| `titanium` | `nebula-titanium.svg` | Neutral, high-contrast alternative; palette 21 |

The default is silver violet: it retains the soft character without a bright
candy color or a generic black-and-white tool identity. The dark alternative
uses graphite rather than saturated purple. These are design judgments, not
claims of exclusive colors or completed trademark clearance.

### Switching

GPUI Settings → Appearance → App icon writes `app_icon=` to
`nebula_settings.txt`. Missing, invalid and retired values fall back to silver
violet. Appearance reset restores the default; appearance backup includes the
selection. Icon selection is independent of terminal/system theme selection.

On Windows, selection updates open window icons (including Alt+Tab), the
tray, and in-app previews. New windows and subsequent notifications use the
selection too. Window icons use per-window DPI and are reapplied when the
window's scale changes. Existing pinned shortcuts, Explorer's EXE icon and
the installer keep the default brand icon; no shortcut, executable or system
icon cache is rewritten. Linux/macOS currently change only the in-app preview;
their Dock/launcher icon remains the packaged default.

### Clarity and resource budget

Each ICO contains directly rendered 32-bit RGBA PNG frames at 16, 20, 24, 32,
40, 48, 64, 96, 128 and 256 physical pixels. The 16/20/24 frames use a slightly
larger mark (100 rather than 90 SVG units) and an 11-unit rather than 10-unit
cutout stroke. The tile bounds and symbol paths are unchanged. This is optical
sizing, not upscaling a small bitmap. Higher sizes preserve the master geometry.

PNG IDAT data is recompressed losslessly; no palette quantization or alpha
reduction is used. All three ICOs together must remain below 64 KiB, enforced
by the generator and tests. Native icon handles are reused in a bounded cache
and owned tray handles are destroyed when replaced.

Windows embeds exactly three icon groups. In-app, tray and notification PNG
bytes are read from those same native resources, not a second embedded PNG
collection. Non-Windows builds embed the same three ICOs. `nebula.png` and
the three 1024px PNG exports are packaging/design assets, not extra GPUI
runtime embeds. `nebula.ico` is a byte-identical alias of `nebula-light.ico`;
only the former is linked as the default icon group.

Regenerate using Node.js 24+ with `@resvg/resvg-js` (verified with 2.6.2):

```sh
node scripts/render-app-icons.mjs /path/to/renderer/package.json
python3 -m unittest scripts.tests.test_app_icons -v
cargo test -p nebula-settings --offline --locked
cargo test -p nebula --bin nebula --features gpui-shell app_icon::tests
```

The renderer is development-only; it adds no application dependency. The
optional argument locates a separate renderer installation. The default
`nebula.ico` and `nebula.png` exports always remain silver violet.

### Design archive

The 24-color exploration remains in
`docs/design/icon-color-lab-2026-09-05/index.html`. Original sage SVG masters
remain in `docs/design/icon-final-2026-09-05/archive/`. Neither archive is
embedded or copied by the Windows release asset manifest. Strong-green,
saturated confectionery-purple and other low-value recolors are deliberately
absent from the application picker, not deleted from the design history.

## Third-Party Logo Assets

### Grok

`ai_grok_dark.png` and `ai_grok_light.png` are unmodified copies of
`Grok_Logomark_Dark.png` and `Grok_Logomark_Light.png` from the official xAI
asset archive:

- Brand guidelines: https://x.ai/legal/brand-guidelines
- Asset archive: https://data.x.ai/logos/xAI_Grok_Assets.zip
- Archive SHA-256: `F41A93923A85047B4B5A9571B7EC73339F562C3E58ACD096E25584AB0AE2A1FB`
- Dark PNG SHA-256: `37DDBCB6E2A7F2E4B3BE78A7D41296A3BC7EDF6926362434EFC00DF5A56A3586`
- Light PNG SHA-256: `359056EE8983CFA0BA7E72795078C7C0DDF6C5D7A1870401AB960ED4F9DF9E53`

xAI owns the trademark, intellectual property, and branding rights in xAI,
Grok, and their logos. The xAI Brand Guidelines are usage terms, not an
open-source asset license. These files are used only to identify a running
`grok` or `grok-cli` process and do not imply xAI endorsement, approval, or
sponsorship. The application selects the supplied dark or light file for
contrast and scales it only when rendering; the committed files are unchanged.
