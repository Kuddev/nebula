from pathlib import Path
import re
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[2]
DOCUMENTS = (
    "CONTRIBUTING.md", "docs/project-constraints.md", "docs/architecture.md",
    "docs/architecture-decisions.md", "docs/engineering-evidence.md",
    "docs/internationalization.md",
)


class GovernanceTests(unittest.TestCase):
    def ignored_paths(self, paths):
        result = subprocess.run(
            ["git", "check-ignore", "--no-index", "--stdin"], cwd=ROOT,
            input="\n".join(paths) + "\n", text=True, encoding="utf-8",
            capture_output=True,
        )
        self.assertIn(result.returncode, (0, 1), result.stderr)
        return set(result.stdout.splitlines())

    def test_contributor_documents_exist_and_are_not_ignored(self):
        for name in DOCUMENTS:
            with self.subTest(name=name):
                self.assertTrue((ROOT / name).is_file())
        self.assertEqual(self.ignored_paths(DOCUMENTS), set(), "public contributor docs must not be ignored")

    def test_private_investigations_remain_ignored(self):
        paths = (
            "docs/private-product-comparison.md", "docs/new-investigation/README.md",
            "docs/screenshots/vendor-gap-analysis.md", "docs/release-notes/vendor-study.md",
            "docs/screenshots/vendor-research.md", "docs/screenshots/vendor-competitor-notes.md",
            "docs/screenshots/research/raw-capture.json",
            "docs/release-notes/reference-projects/vendor/src/main.rs",
            "docs/screenshots/external-probes/inspect.ps1",
            "research/vendor/README.md", "reference-projects/vendor/src/main.rs",
            "external-probes/inspect-vendor.py", "impeccable/research.md",
        )
        self.assertEqual(self.ignored_paths(paths), set(paths))

    def test_html_prototypes_do_not_escape_through_public_doc_directories(self):
        paths = (
            "settings.html", "settings.htm", "docs/design/settings/index.html",
            "docs/screenshots/settings.html", "docs/release-notes/demo.htm",
            "scripts/design/settings-prototype.html", "tools/design/settings_mockup.html",
            "scripts/design/settings-prototype.htm", "tools/design/settings_mockup.htm",
            "prototypes/sidebar/assets/preview.png", "scripts/render_shell_icon.html",
        )
        self.assertEqual(self.ignored_paths(paths), set(paths))

    def test_scratch_scripts_and_caches_remain_ignored(self):
        paths = (
            "tmp/test-expanded-icons-20260905.ps1", "tmp/check-tool.py",
            "tmp/vendor-probe/main.rs", "tmp-check.ps1", "tmp_check.py",
            "scratch/inspect-window.ps1", ".tmp-check.py", ".probe-round/probe.ps1",
            "scripts/ghost_repro/probe.ps1", "scripts/drawer_repro.ps1",
            "scripts/__pycache__/helper.cpython-313.pyc", "tools/diagnostic.pyc",
            "scripts/diagnostic.pyo", ".pytest_cache/v/cache/nodeids",
            ".mypy_cache/check.json", ".ruff_cache/cache.bin",
        )
        self.assertEqual(self.ignored_paths(paths), set(paths))

    def test_maintained_tests_dependencies_and_public_assets_stay_visible(self):
        paths = (
            *DOCUMENTS, ".github/CODEOWNERS", ".github/PULL_REQUEST_TEMPLATE.md",
            ".github/workflows/architecture.yml", "architecture/dependencies.toml",
            "architecture/file-budgets.txt", "scripts/check_architecture.py",
            "scripts/tests/test_architecture_governance.py", "scripts/tests/new_regression.py",
            "scripts/conformance/tests/test_harness.py", "scripts/launch_probe_instance.ps1",
            "scripts/probe_ssh_connect.ps1", "tools/probe_ime.rs",
            "tools/i18n-contract/Cargo.toml", "tools/i18n-contract/Cargo.lock",
            "nebula_app/tests/i18n_contract.rs", "nebula_app/tests/fixtures/page.html",
            "nebula_app/i18n/fr-FR.json", "third_party/winit-0.30.13/Cargo.toml",
            "third_party/winit-0.30.13/src/lib.rs", "docs/screenshots/SHOTLIST.md",
            "docs/screenshots/hero.png", "docs/release-notes/v1.5.0.md",
            "docs/skills/nebula-runtime/SKILL.md",
        )
        self.assertEqual(self.ignored_paths(paths), set())

    def test_relative_document_links_resolve(self):
        for name in DOCUMENTS:
            document = ROOT / name
            text = document.read_text(encoding="utf-8")
            for link in re.findall(r"\]\(([^)]+)\)", text):
                if "://" in link or link.startswith("#"):
                    continue
                target = link.split("#", 1)[0]
                with self.subTest(document=name, target=target):
                    self.assertTrue((document.parent / target).exists())

    def test_required_job_is_unfiltered_and_unprivileged(self):
        workflow = (ROOT / ".github/workflows/architecture.yml").read_text(encoding="utf-8")
        for required in ("  pull_request:", "  merge_group:", "contents: read",
                         "name: architecture-contracts", "persist-credentials: false",
                         "fetch-depth: 0", "--base", "tools/i18n-contract/Cargo.toml --locked",
                         "scripts.tests.test_architecture_governance"):
            with self.subTest(required=required):
                self.assertIn(required, workflow)
        for unsafe in ("pull_request_target:", "continue-on-error:", "paths:", "paths-ignore:"):
            with self.subTest(unsafe=unsafe):
                self.assertNotIn(unsafe, workflow)

    def test_review_and_activation_contracts_are_documented(self):
        owners = (ROOT / ".github/CODEOWNERS").read_text(encoding="utf-8")
        self.assertRegex(owners, r"(?m)^\*\s+@[A-Za-z0-9]")
        template = (ROOT / ".github/PULL_REQUEST_TEMPLATE.md").read_text(encoding="utf-8")
        self.assertIn("CONTRIBUTING.md", template)
        self.assertIn("counterexample", template)
        constraints = (ROOT / "docs/project-constraints.md").read_text(encoding="utf-8")
        self.assertIn("do **not** activate GitHub merge protection", constraints)
        self.assertIn("Empty/invalid budgets", constraints)
        self.assertIn("an author cannot approve their own PR", constraints)


if __name__ == "__main__":
    unittest.main()
