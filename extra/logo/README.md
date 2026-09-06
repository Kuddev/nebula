# Logo Assets

## Nebula App Icon

The rounded, asymmetric symbol and cutout prompt are shared by 25 colorways.
Titanium (palette 21) is the default application and README identity. The
display name is Pebrel; asset paths retain Nebula for compatibility. The original 01–24 palettes are available as
personal choices, with graphite violet retained as palette 25.

| Setting | Source | Role |
| --- | --- | --- |
| `titanium` | `nebula-titanium.svg` | Default identity; palette 21 |
| `silver-violet` | `nebula-light.svg` | Optional silver-violet colorway; palette 12 |
| `graphite-violet` | `nebula-dark.svg` | Optional graphite / gray-violet colorway; palette 25 |

`nebula_settings/src/app_icon.rs` is the canonical catalog of stable setting
keys, original palette numbers, bilingual names and colors. The renderer
exports `icon-catalog.json` from it. Personal color options do not imply
exclusive colors or completed trademark clearance.

### Switching

GPUI Settings → Appearance → App icon writes `app_icon=` to
`nebula_settings.txt`. Missing or invalid values fall back to Titanium.
Existing valid selections remain unchanged. The picker initially shows the
three colorways above and the current selection, with a button to show all 25.
Appearance reset restores the default; appearance backup includes the
selection. Icon selection is independent of terminal/system theme selection.

On Windows, selection updates open window icons (including Alt+Tab), the
tray, and in-app previews. New windows and subsequent notifications use the
selection too. Window icons use per-window DPI and are reapplied when the
window's scale changes. Existing pinned shortcuts, Explorer's EXE icon and
the installer keep the default brand icon; no shortcut, executable or system
icon cache is rewritten. Linux/macOS currently change only the in-app preview;
their Dock/launcher icon remains the packaged default.

### Clarity and resource budget

The default ICO contains 32-bit RGBA PNG frames at 16, 20, 24, 28, 32, 40, 48,
56, 64, 80, 96, 112, 128 and 256 physical pixels. The shared coverage atlas
contains these sizes plus a 512px frame. Below 64px, the mark gradually
increases from 90 to 108 SVG units, with a pixel-fitted prompt and a minimum
1.5px cutout stroke. The underline is aligned to the physical pixel grid.
The tile and outer symbol paths stay unchanged; 64px and larger preserve
the original master geometry.

The generator renders at 8× resolution for 16–64px and 4× above that, then
averages premultiplied coverage before applying colors. This avoids colored
transparent fringes and sharpening halos. Nonstandard sizes resample coverage
with a non-ringing triangle filter before colorization. In-app previews request
their actual logical size times the window scale factor; the tray receives
exact-size RGBA pixels without a second resize. Antialiasing still produces
partially covered edge pixels, and shell/browser scaling can affect appearance.

Windows embeds one default icon group and one shared coverage atlas, not 25
complete ICOs or PNG collections. Palette colors are applied at runtime using
the existing image dependency. Non-Windows in-app previews use the same atlas.
The combined Windows image payload is 80,402 bytes (78.52 KiB), below the
96 KiB budget enforced by the generator and tests. This measures image data,
not installer growth. PNG/preview caches have a 128-entry limit; native window
icon handles are reused, and owned tray handles are destroyed when replaced.

`nebula.ico` is byte-identical to `nebula-titanium.ico`, and `nebula.png` to
`nebula-titanium.png`. Alternate ICOs and 1024px PNG exports are design assets,
not extra GPUI runtime embeds.

Regenerate using Node.js 24+ with `@resvg/resvg-js` (verified with 2.6.2):

```sh
node scripts/render-app-icons.mjs /path/to/renderer/package.json
node scripts/render-app-icons.mjs /path/to/renderer/package.json --check
python3 -m unittest scripts.tests.test_app_icons -v
cargo test -p nebula-settings --offline --locked
cargo test -p nebula --bin nebula --features gpui-shell app_icon::tests
```

The renderer is development-only; it adds no application dependency. The
optional argument locates a separate renderer installation. The default
`nebula.ico` and `nebula.png` exports always remain Titanium.

### Design archive

The local 24-color exploration remains in
`docs/design/icon-color-lab-2026-09-05/index.html`. The updated local preview at
`docs/design/icon-all-palettes-2026-09-05/index.html` shows all 25 choices on
light/dark surfaces and a before/after small-size comparison. These are PNG
contact sheets, not native taskbar screenshots. Original sage SVG masters
remain in `docs/design/icon-final-2026-09-05/archive/`. The historical pages
do not define the current default. These local design archives are ignored
by Git and are not required by the asset tests.

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

### Google Antigravity

`ai_antigravity.png` is an unmodified copy of the full-color product icon from
Google's Antigravity press assets, verified on 2026-09-06:

- Press assets: https://antigravity.google/press/
- PNG: https://antigravity.google/assets/image/brand/antigravity-icon__full-color.png
- Dimensions: 540 x 540, with transparency
- SHA-256: `E0CD08CCD10CD8D08CCF0BA449823EE88495825C0841619618100D3AB089F51E`

Google retains rights to its product names and logos. Publication in a press kit
does not make the logo an open-source asset or imply endorsement. The icon is
used to identify running `antigravity`, `antigravity-cli` and `agy` processes.
Both UI shells keep its original colors on light and dark themes and resize it
only for rendering. No LobeHub asset is embedded for Antigravity.
