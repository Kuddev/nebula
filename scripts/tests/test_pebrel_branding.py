import re
import tomllib
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


class PebrelBrandingTests(unittest.TestCase):
    def source(self, path):
        return (ROOT / path).read_text(encoding="utf-8-sig")

    def manifest(self, path):
        return tomllib.loads(self.source(path))

    def test_visible_identity_and_cli_use_pebrel(self):
        brand = self.source("nebula_app/src/brand.rs")
        cli = self.source("nebula_app/src/cli.rs")
        self.assertIn('pub const NAME: &str = "Pebrel";', brand)
        self.assertIn('bin_name = "pebrel"', cli)
        self.assertIn('display_name = crate::brand::NAME', cli)
        self.assertIn('version = env!("VERSION")', cli)
        self.assertEqual(
            [target["name"] for target in self.manifest("nebula_app/Cargo.toml")["bin"]],
            ["pebrel"],
        )
        self.assertEqual(
            [target["name"] for target in self.manifest("nebula_hook/Cargo.toml")["bin"]],
            ["pebrel-hook"],
        )
        window = self.source("nebula_app/src/config/window.rs")
        self.assertIn('DEFAULT_NAME: &str = crate::brand::NAME;', window)
        self.assertIn('DEFAULT_CLASS: &str = "Pebrel";', window)
        gpui = self.source("nebula_app/src/gpui_shell/workspace/windowing.rs")
        self.assertIn('window.set_window_title(crate::brand::NAME)', gpui)
        self.assertIn('app_id: Some("pebrel".to_owned())', gpui)
        self.assertIn('app_id: Some("pebrel-quick-terminal".to_owned())', gpui)
        welcome = self.source("nebula_app/src/window_context/welcome.rs")
        self.assertIn('crate::brand::NAME', welcome)
        self.assertNotIn('Nebula Terminal', welcome)

    def test_installer_migrates_names_and_keeps_upgrade_identity(self):
        installer = self.source("scripts/installer.iss")
        for expected in (
            "AppName=Pebrel",
            "AppId={{61022144-7D0A-4E54-94F2-C329A8F58656}",
            "DefaultDirName={code:DefaultInstallDir}",
            "UsePreviousAppDir=no",
            r"Software\Pebrel",
            r"App Paths\pebrel.exe",
            r"shell\Pebrel",
            r'Source: "{#BuildRoot}\pebrel.exe";',
            r'Source: "{#BuildRoot}\pebrel-hook.exe";',
            'RunOnceId: "RemovePebrelAiHooks"',
            'english.OpenInPebrel=Open in Pebrel',
            'chinesesimplified.OpenInPebrel=在 Pebrel 中打开',
            '#include "installer-migration.iss"',
        ):
            with self.subTest(expected=expected):
                self.assertIn(expected, installer)
        migration = self.source("scripts/installer-migration.iss")
        self.assertIn(r"{localappdata}\Programs\Pebrel", migration)
        self.assertIn(r"Software\Nebula Terminal", migration)

    def test_pebrel_assets_are_default_and_keep_explicit_legacy_alias(self):
        for path in ("scripts/package-release.ps1", "scripts/build-installer.ps1"):
            source = self.source(path)
            with self.subTest(path=path):
                self.assertIn("[ValidateSet('NebulaTerminal', 'Pebrel')]", source)
                self.assertIn("$PackageBrand = 'Pebrel'", source)
                self.assertIn(".VersionInfo.ProductName -ne 'Pebrel'", source)
                self.assertIn("gpui-shell", source)
                self.assertIn("Stale binary:", source)
        installer = self.source("scripts/installer.iss")
        self.assertIn('#define PackageBrand "Pebrel"', installer)
        self.assertIn("OutputBaseFilename={#PackageBrand}-v{#AppVersion}-windows-x64-setup", installer)
        builder = self.source("scripts/build-installer.ps1")
        self.assertIn('"/DPackageBrand=$PackageBrand"', builder)

    def test_update_asset_validation_shares_current_and_legacy_names(self):
        check = self.source("nebula_app/src/update_check.rs")
        download = self.source("nebula_app/src/update_download.rs")
        self.assertIn('format!("Pebrel-{version}-windows-x64-setup.exe")', check)
        self.assertIn('format!("Pebrel-v{version}-windows-x64-setup.exe")', check)
        self.assertIn('format!("NebulaTerminal-{version}-windows-x64-setup.exe")', check)
        self.assertIn("windows_x64_installer_names(version)", check)
        self.assertIn("windows_x64_installer_names(&asset.version).contains(&asset.name)", download)

    def test_notification_identity_uses_pebrel(self):
        source = self.source("nebula_app/src/platform/notifications.rs")
        self.assertIn('AUMID: &str = "com.pebrel.terminal";', source)
        self.assertIn('set_reg_sz(&subkey, "DisplayName", crate::brand::NAME)', source)

    def test_config_directory_uses_pebrel_and_migrates_legacy_data(self):
        paths = self.source("nebula_settings/src/paths.rs")
        self.assertIn('"PEBREL_CONFIG_DIR", "NEBULA_CONFIG_DIR"', paths)
        self.assertIn('default_dir("Pebrel")', paths)
        self.assertIn('default_dir("Nebula")', paths)
        self.assertIn('"pebrel_settings.txt"', paths)
        migration = self.source("nebula_settings/src/paths/migration.rs")
        self.assertIn('("nebula_settings.txt", "pebrel_settings.txt")', migration)
        startup = self.source("nebula_app/src/main.rs")
        self.assertIn("nebula_settings::migrate_legacy_data()", startup)

    def test_project_identity_without_local_preview_notice(self):
        readme = self.source("README.md")
        self.assertIn('<h1 align="center">Pebrel</h1>', readme)
        self.assertIn("Pebrel (formerly Nebula)", readme)
        self.assertIn("https://github.com/Kuddev/pebrel/releases", readme)
        self.assertNotIn("Local branding preview", readme)
        self.assertNotIn("本地品牌预览", readme)
        self.assertNotIn("A name change does not create a new published release", readme)

    def test_current_changelog_matches_bilingual_release_notes(self):
        version = self.manifest("nebula_app/Cargo.toml")["package"]["version"]
        notes = self.source(f"docs/release-notes/v{version}.md")
        self.assertTrue(notes.startswith(f"# Pebrel {version}\n"))
        body = notes.split("## English\n", 1)[1].split("## SHA256", 1)[0]
        expected = re.sub(r"^##(?=#* )", "###", "## English\n" + body, flags=re.MULTILINE)
        changelog = self.source("CHANGELOG.md")
        entry = changelog.split(f"## {version} -", 1)[1].split("\n## ", 1)[0]
        actual = "### English\n" + entry.split("### English\n", 1)[1]
        self.assertEqual(actual.strip(), expected.strip())


if __name__ == "__main__":
    unittest.main()
