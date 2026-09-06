from pathlib import Path
import tempfile
import unittest

from scripts.architecture.dependencies import check


class DependencyTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.workspace()
        self.manifest("app", "application")
        self.manifest("core", "core-model")
        self.manifest("derive", "derive-helper")
        self.policy = {
            "version": 1,
            "renderer_packages": ["gpui", "winit"],
            "crates": {
                "app": {"layer": "application", "dependencies": ["core"]},
                "core": {"layer": "core", "dev-dependencies": ["derive"]},
                "derive": {"layer": "core", "dev-dependencies": ["core"]},
            },
        }

    def workspace(self, extra="", members='"app", "core", "derive"'):
        (self.root / "Cargo.toml").write_text(f"[workspace]\nmembers = [{members}]\n{extra}", encoding="utf-8")

    def manifest(self, member, package, extra=""):
        directory = self.root / member
        directory.mkdir(exist_ok=True)
        (directory / "Cargo.toml").write_text(
            f'[package]\nname = "{package}"\nversion = "0.0.0"\n{extra}', encoding="utf-8"
        )

    def errors(self):
        return check(self.root, self.policy)[0]

    def test_legal_inward_dependency_and_package_rename(self):
        self.manifest("app", "application", '[dependencies]\nmodel = { package = "core-model", path = "../core" }')
        self.assertEqual(self.errors(), [])

    def test_reverse_dependency_is_rejected(self):
        self.manifest("core", "core-model", '[dependencies]\napplication = { path = "../app" }')
        self.assertIn("forbidden dependencies edge to app", self.errors()[0])

    def test_core_cannot_whitelist_a_reverse_production_layer(self):
        self.policy["crates"]["core"]["dependencies"] = ["app"]
        self.manifest("core", "core-model", '[dependencies]\napplication = { path = "../app" }')
        self.assertIn("forbidden production layer direction", self.errors()[0])

    def test_optional_target_and_renamed_renderer_are_checked(self):
        self.manifest("core", "core-model", '''[target.'cfg(windows)'.dependencies]
display_adapter = { package = "gpui", version = "0.2", optional = true }''')
        self.assertIn("renderer dependency gpui", self.errors()[0])

    def test_workspace_inheritance_resolves_paths_from_workspace_root(self):
        self.workspace('[workspace.dependencies]\nmodel = { package = "core-model", path = "core" }')
        self.manifest("app", "application", '[dependencies]\nmodel.workspace = true')
        self.assertEqual(self.errors(), [])

    def test_workspace_inherited_renderer_is_checked(self):
        self.workspace('[workspace.dependencies]\nwindow = { package = "winit", version = "0.30" }')
        self.manifest("core", "core-model", '[dependencies]\nwindow.workspace = true')
        self.assertIn("renderer dependency winit", self.errors()[0])

    def test_unresolved_workspace_dependency_fails(self):
        self.manifest("core", "core-model", '[dependencies]\nmissing.workspace = true')
        with self.assertRaises(ValueError):
            self.errors()

    def test_build_dependencies_are_checked(self):
        self.manifest("core", "core-model", '[build-dependencies]\napplication = { path = "../app" }')
        self.assertIn("forbidden build-dependencies", self.errors()[0])

    def test_production_cycle_is_rejected_even_when_edges_are_allowed(self):
        self.policy["crates"]["core"]["build-dependencies"] = ["app"]
        self.manifest("core", "core-model", '[build-dependencies]\napplication = { path = "../app" }')
        self.manifest("app", "application", '[dependencies]\ncore-model = { path = "../core" }')
        self.assertTrue(any("production dependency cycle" in error for error in self.errors()))

    def test_cargo_legal_dev_cycle_is_not_a_production_cycle(self):
        self.manifest("core", "core-model", '[dev-dependencies]\nderive-helper = { path = "../derive" }')
        self.manifest("derive", "derive-helper", '[dev-dependencies]\ncore-model = { path = "../core" }')
        self.assertEqual(self.errors(), [])

    def test_zero_production_dependencies_allow_test_tools(self):
        self.policy["crates"]["core"]["zero_production_dependencies"] = True
        self.manifest("core", "core-model", '[dev-dependencies]\nproptest = "1"')
        self.assertEqual(self.errors(), [])
        self.manifest("core", "core-model", '[dependencies]\nserde = "1"')
        self.assertIn("zero-dependency contract", self.errors()[0])

    def test_unclassified_workspace_member_is_rejected(self):
        self.manifest("lab", "lab")
        self.workspace(members='"app", "core", "derive", "lab"')
        self.assertIn("classification differ", self.errors()[0])

    def test_unclassified_path_dependency_cannot_hide_an_edge(self):
        self.manifest("app", "application", '[dependencies]\nmodel = { path = "../unknown" }')
        self.assertIn("unclassified path dependency", self.errors()[0])

    def test_mismatched_package_and_path_is_rejected(self):
        self.manifest("app", "application", '[dependencies]\nderive-helper = { path = "../core" }')
        self.assertIn("package/path mismatch", self.errors()[0])

    def test_comment_or_module_alias_is_not_a_dependency(self):
        self.manifest("core", "core-model", '# crate::gpui and #[path = "../display.rs"] are not dependencies')
        (self.root / "core/source.rs").write_text('const TEXT: &str = "gpui::Window";', encoding="utf-8")
        self.assertEqual(self.errors(), [])

    def test_invalid_toml_and_unknown_policy_fields_fail(self):
        self.policy["crates"]["core"]["zero_dependenices"] = True
        self.assertIn("unknown dependency rule field", self.errors()[0])
        (self.root / "core/Cargo.toml").write_text("[broken", encoding="utf-8")
        with self.assertRaises(ValueError):
            self.errors()

    def test_unhandled_workspace_layout_fails_instead_of_skipping(self):
        for extra, members in [('exclude = ["other"]', '"app", "core", "derive"'),
                               ("", '"crates/*"'), ("", '"../outside"')]:
            with self.subTest(extra=extra, members=members):
                self.workspace(extra, members)
                with self.assertRaises(ValueError):
                    self.errors()

    def test_empty_or_duplicate_workspace_cannot_pass(self):
        for members in ("", '"app", "app"'):
            with self.subTest(members=members):
                self.workspace(members=members)
                with self.assertRaises(ValueError):
                    self.errors()

    def test_custom_source_paths_cannot_escape_coverage(self):
        self.manifest("core", "core-model", '[lib]\npath = "../outside.rs"')
        self.assertIn("invalid custom source path", self.errors()[0])

    def test_same_package_from_registry_is_not_a_workspace_edge(self):
        self.manifest("app", "application", '[dev-dependencies]\nprevious = { package = "core-model", version = "=0.9.0" }')
        self.assertEqual(self.errors(), [])

    def test_shared_custom_source_is_allowed_inside_scanned_roots(self):
        (self.root / "derive/shared.rs").write_text("", encoding="utf-8")
        self.manifest("core", "core-model", '[lib]\npath = "../derive/shared.rs"')
        self.assertEqual(self.errors(), [])

    def test_custom_target_inside_excluded_output_is_rejected(self):
        (self.root / "core/target").mkdir()
        (self.root / "core/target/real.rs").write_text("", encoding="utf-8")
        self.manifest("core", "core-model", '[lib]\npath = "target/real.rs"')
        errors = check(self.root, self.policy, scanned_paths=set())[0]
        self.assertIn("invalid custom source path", errors[0])

    def test_symlinked_workspace_ancestor_is_rejected(self):
        try:
            (self.root / "redirect").symlink_to(self.root, target_is_directory=True)
        except OSError as error:
            self.skipTest(f"symlink unavailable: {error}")
        self.workspace(members='"redirect/core"')
        with self.assertRaises(ValueError):
            self.errors()


if __name__ == "__main__":
    unittest.main()
