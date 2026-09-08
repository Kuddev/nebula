from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from scripts.ci import (
    ROOT, affected_packages, cargo_commands, changed_paths, plan, run_tests, workspace_graph,
)


class SelectionTests(unittest.TestCase):
    def setUp(self):
        self.names, self.consumers = workspace_graph(ROOT)

    def selected(self, *paths):
        return affected_packages(paths, self.names, self.consumers)[0]

    def test_product_change_runs_product_without_unrelated_lab(self):
        self.assertEqual(self.selected("nebula_app/src/gpui_shell/terminal/view.rs"), {"nebula"})

    def test_terminal_change_also_runs_consumer(self):
        self.assertEqual(self.selected("nebula_terminal/src/term/keyboard.rs"),
                         {"nebula_terminal", "nebula"})

    def test_settings_build_dependency_retests_product(self):
        self.assertEqual(self.selected("nebula_settings/src/languages.rs"),
                         {"nebula-settings", "nebula"})

    def test_config_dev_cycle_terminates_and_retests_derive(self):
        self.assertEqual(self.selected("nebula_config/src/lib.rs"),
                         {"nebula_config", "nebula_config_derive", "nebula"})

    def test_hook_does_not_invent_a_dependency_on_ui(self):
        self.assertEqual(self.selected("nebula_hook/src/main.rs"), {"nebula_hook"})

    def test_application_assets_and_lua_are_not_docs_only(self):
        for path in ("nebula_app/i18n/fr-FR.json", "nebula_app/res/icon.svg",
                     "nebula_app/src/config/templates/pebrel.en-US.lua"):
            with self.subTest(path=path):
                self.assertEqual(self.selected(path), {"nebula"})

    def test_manifests_toolchain_patches_and_unknown_paths_run_every_crate(self):
        for path in ("Cargo.toml", "nebula_app/Cargo.toml", "Cargo.lock",
                     "tools/i18n-contract/Cargo.lock", "rust-toolchain.toml",
                     "third_party/winit-0.30.13/src/lib.rs", ".cargo/config.toml",
                     ".github/ci-profile.toml", "scripts/ci.py", "unknown/input.bin"):
            with self.subTest(path=path):
                self.assertEqual(self.selected(path), set(self.names.values()))

    def test_docs_change_keeps_native_contracts_without_rust_compilation(self):
        with patch("scripts.ci.changed_paths", return_value=["README.md"]):
            matrix, _ = plan(ROOT, "base", "HEAD", "pull_request")
        self.assertEqual(len(matrix["include"]), 4)
        self.assertTrue(all(job["packages"] == [] and job["suite"] == "core"
                            for job in matrix["include"]))

    def test_initial_push_and_unavailable_history_fall_back_to_full(self):
        for base in ("", "0" * 40, "unavailable-ci-base"):
            with self.subTest(base=base):
                matrix, reason = plan(ROOT, base, "HEAD", "push")
                self.assertEqual(len(matrix["include"]), 8)
                self.assertIn("fallback", reason)
                packages = {p for job in matrix["include"] for p in job["packages"]}
                self.assertEqual(packages, set(self.names.values()))

    def test_full_and_weekly_runs_cover_all_crates_and_architectures(self):
        for event, full in (("schedule", False), ("workflow_call", True)):
            matrix, _ = plan(ROOT, "HEAD", "HEAD", event, full)
            self.assertEqual(len(matrix["include"]), 8)
            platforms = {job["platform"] for job in matrix["include"]}
            self.assertEqual(platforms, {"linux-x64", "windows-x64", "macos-arm64", "macos-x64"})

    def test_windows_recovery_does_not_rebuild_other_platforms(self):
        matrix, _ = plan(ROOT, "", "HEAD", "workflow_call", full=True, windows_only=True)
        self.assertEqual(len(matrix["include"]), 2)
        self.assertEqual({job["platform"] for job in matrix["include"]}, {"windows-x64"})

    def test_aliases_inherited_paths_and_target_dev_edges_use_shared_resolver(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.toml").write_text('''
[workspace]
members = ["core", "app"]
[workspace.dependencies]
renamed = { package = "core-package", path = "core" }
''', encoding="utf-8")
            for member, text in (("core", '[package]\nname = "core-package"\n'),
                                 ("app", '''
[package]
name = "app-package"
[target.'cfg(windows)'.dev-dependencies]
renamed.workspace = true
''')):
                (root / member).mkdir()
                (root / member / "Cargo.toml").write_text(text, encoding="utf-8")
            names, consumers = workspace_graph(root)
            self.assertEqual(affected_packages(["core/src/lib.rs"], names, consumers)[0],
                             {"core-package", "app-package"})


class GitDiffTests(unittest.TestCase):
    def test_moves_select_both_owners_and_push_covers_all_commits(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            def git(*args):
                return subprocess.check_output(["git", *args], cwd=root, text=True,
                                               encoding="utf-8", stderr=subprocess.PIPE).strip()

            git("init", "--quiet")
            git("config", "user.name", "CI fixture")
            git("config", "user.email", "ci@example.invalid")
            (root / "old.rs").write_text("old", encoding="utf-8")
            git("add", "old.rs")
            git("commit", "--quiet", "-m", "base")
            base = git("rev-parse", "HEAD")
            git("mv", "old.rs", "renamed.rs")
            git("commit", "--quiet", "-m", "move")
            (root / "second.rs").write_text("new", encoding="utf-8")
            git("add", "second.rs")
            git("commit", "--quiet", "-m", "second")
            self.assertEqual(set(changed_paths(root, base, "HEAD", "push")),
                             {"old.rs", "renamed.rs", "second.rs"})
            self.assertEqual(changed_paths(root, "HEAD", "HEAD", "push"), [])


class TestExecutionTests(unittest.TestCase):
    def test_product_checks_normal_features_and_runs_all_tests_once(self):
        commands = cargo_commands(["nebula", "nebula-gpui"])
        self.assertEqual([command[1] for command in commands], ["check", "test"])
        self.assertIn("gpui-shell", commands[0])
        self.assertIn("nebula/gpui-test-support", commands[1])
        self.assertNotIn("dialog", commands[1])
        self.assertNotIn("--lib", commands[1])
        self.assertNotIn("--bin", commands[1])
        self.assertTrue(all("--locked" in command for command in commands))

    def test_core_suite_does_not_load_product_features(self):
        commands = cargo_commands(["nebula_terminal"])
        self.assertEqual(len(commands), 1)
        self.assertNotIn("--features", commands[0])

    def test_empty_selection_never_defaults_to_workspace_test(self):
        self.assertEqual(cargo_commands([]), [])

    def test_unknown_packages_cannot_become_cargo_options(self):
        for packages in (["--workspace"], ["unknown"], "nebula"):
            with self.subTest(packages=packages), self.assertRaises(ValueError):
                cargo_commands(packages)

    def test_failed_compile_stops_tests_and_propagates_exit_status(self):
        with patch("scripts.ci.subprocess.run", return_value=subprocess.CompletedProcess([], 23)) as run:
            with patch("scripts.ci.append_summary"):
                self.assertEqual(run_tests(["nebula"]), 23)
            self.assertEqual(run.call_count, 1)

    def test_profile_overrides_all_optimized_dev_dependencies(self):
        import tomllib
        workspace = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
        config = tomllib.loads((ROOT / ".github/ci-profile.toml").read_text(encoding="utf-8"))
        profile = config["profile"]["ci"]
        self.assertEqual(profile["debug"], 0)
        for package in workspace["profile"]["dev"]["package"]:
            with self.subTest(package=package):
                self.assertEqual(profile["package"][package]["opt-level"], 0)


if __name__ == "__main__":
    unittest.main()
