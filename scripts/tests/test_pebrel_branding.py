import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


class PebrelBrandingTests(unittest.TestCase):
    def source(self, path):
        return (ROOT / path).read_text(encoding="utf-8-sig")

    def test_visible_identity_keeps_legacy_cli(self):
        brand = self.source("nebula_app/src/brand.rs")
        cli = self.source("nebula_app/src/cli.rs")
        self.assertIn('pub const NAME: &str = "Pebrel";', brand)
        self.assertIn('bin_name = "nebula"', cli)
        self.assertIn('display_name = crate::brand::NAME', cli)
        self.assertIn('version = env!("VERSION")', cli)
        window = self.source("nebula_app/src/config/window.rs")
        self.assertIn('DEFAULT_NAME: &str = crate::brand::NAME;', window)
        self.assertIn('DEFAULT_CLASS: &str = "Nebula";', window)
        gpui = self.source("nebula_app/src/gpui_shell/workspace/windowing.rs")
        self.assertIn('window.set_window_title(crate::brand::NAME)', gpui)
        self.assertIn('app_id: Some("nebula".to_owned())', gpui)
        welcome = self.source("nebula_app/src/window_context/welcome.rs")
        self.assertIn('crate::brand::NAME', welcome)
        self.assertNotIn('Nebula Terminal', welcome)

    def test_installer_keeps_upgrade_identity(self):
        installer = self.source("scripts/installer.iss")
        for expected in (
            "AppName=Pebrel",
            "AppId={{61022144-7D0A-4E54-94F2-C329A8F58656}",
            r"DefaultDirName={localappdata}\Programs\Nebula Terminal",
            r"Software\Nebula Terminal",
            r"App Paths\nebula.exe",
            r"shell\NebulaTerminal",
            'RunOnceId: "RemoveNebulaAiHooks"',
            'english.OpenInNebula=Open in Pebrel',
            'chinesesimplified.OpenInNebula=在 Pebrel 中打开',
        ):
            with self.subTest(expected=expected):
                self.assertIn(expected, installer)

    def test_local_asset_brand_is_explicit(self):
        for path in ("scripts/package-release.ps1", "scripts/build-installer.ps1"):
            source = self.source(path)
            with self.subTest(path=path):
                self.assertIn("[ValidateSet('NebulaTerminal', 'Pebrel')]", source)
                self.assertIn("$PackageBrand = 'NebulaTerminal'", source)
                self.assertIn(".VersionInfo.ProductName -ne 'Pebrel'", source)
                self.assertIn("gpui-shell", source)
                self.assertIn("Stale binary:", source)
        installer = self.source("scripts/installer.iss")
        self.assertIn('#define PackageBrand "NebulaTerminal"', installer)
        self.assertIn("OutputBaseFilename={#PackageBrand}-{#AppVersion}-windows-x64-setup", installer)
        builder = self.source("scripts/build-installer.ps1")
        self.assertIn('"/DPackageBrand=$PackageBrand"', builder)

    def test_existing_update_asset_contract_is_unchanged(self):
        check = self.source("nebula_app/src/update_check.rs")
        download = self.source("nebula_app/src/update_download.rs")
        self.assertIn('format!("NebulaTerminal-{version}-windows-x64-setup.exe")', check)
        self.assertIn('format!("NebulaTerminal-{}-windows-x64-setup.exe", asset.version)', download)
        self.assertNotIn("Pebrel-", check)
        self.assertNotIn("Pebrel-", download)

    def test_notification_identity_is_not_renamed(self):
        source = self.source("nebula_app/src/platform/notifications.rs")
        self.assertIn('AUMID: &str = "com.nebula.terminal";', source)
        self.assertIn('set_reg_sz(&subkey, "DisplayName", crate::brand::NAME)', source)

    def test_config_directory_contract_is_unchanged(self):
        settings = self.source("nebula_settings/src/lib.rs")
        self.assertIn('var_os("NEBULA_CONFIG_DIR")', settings)
        self.assertIn('.join("Nebula")', settings)
        self.assertNotIn('.join("Pebrel")', settings)

    def test_project_identity_without_local_preview_notice(self):
        changelog = self.source("CHANGELOG.md").split("## 1.5.0 -", 1)[0]
        self.assertIn("Pebrel", changelog)
        self.assertIn("not a new published release", changelog)
        self.assertIn("不代表新的已发布版本", changelog)
        readme = self.source("README.md")
        self.assertIn('<h1 align="center">Pebrel</h1>', readme)
        self.assertIn("Pebrel (formerly Nebula)", readme)
        self.assertIn("https://github.com/Kuddev/nebula/releases", readme)
        self.assertNotIn("Local branding preview", readme)
        self.assertNotIn("本地品牌预览", readme)
        self.assertNotIn("A name change does not create a new published release", readme)


if __name__ == "__main__":
    unittest.main()
