from pathlib import Path
import io
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from scripts.architecture.budgets import Budget, check, line_count, source_files
from scripts import check_architecture


class LineBudgetTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "app/src").mkdir(parents=True)

    def write(self, name, count):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"source\n" * count)

    def policy(self, exceptions="", limit=2000):
        return Budget.parse(f"limit {limit}\nroot app\n{exceptions}")

    def test_physical_lines(self):
        for content, expected in [(b"", 0), (b"\n", 1), (b"a\nb", 2),
                                  (b"a\r\nb\r\n", 2), ("中文\n".encode(), 1),
                                  (b"vertical\vtab", 1)]:
            with self.subTest(content=content):
                self.assertEqual(line_count(content), expected)

    def test_soft_target_is_not_a_failure(self):
        for count in (0, 1, 800, 801, 2000):
            with self.subTest(count=count):
                self.write("app/src/view.rs", count)
                self.assertEqual(check(self.root, self.policy())[0], [])

    def test_hard_limit_includes_scripts_and_untracked_files(self):
        for suffix in ("rs", "py", "ps1", "mjs", "js", "sh", "lua"):
            with self.subTest(suffix=suffix):
                self.write(f"app/src/new.{suffix}", 2001)
                self.assertTrue(check(self.root, self.policy())[0])

    def test_exact_exception_and_tightening_notice(self):
        self.write("app/src/legacy.rs", 2100)
        errors, notices, _ = check(self.root, self.policy("app/src/legacy.rs 2101"))
        self.assertEqual(errors, [])
        self.assertIn("tighten exception to 2100", notices[0])
        self.assertEqual(check(self.root, self.policy("app/src/legacy.rs 2100"))[0], [])

    def test_deleted_and_shrunk_exceptions_fail(self):
        budget = self.policy("app/src/legacy.rs 2100")
        self.write("app/src/other.rs", 1)
        self.assertIn("missing-source", check(self.root, budget)[0][0])
        self.write("app/src/legacy.rs", 2000)
        self.assertIn("remove the exception", check(self.root, budget)[0][0])

    def test_duplicate_and_invalid_records_fail(self):
        for text in (
            "limit 2000\nlimit 2000\nroot app", "root app", "limit 0\nroot app",
            "limit 2000\nroot app\nroot app", "limit 2000\nroot ../app", "limit ٢٠٠٠\nroot app",
            "limit 2000\nroot /app", "limit 2000\nroot D:/app",
            "limit 2000\nroot app\nroot app/src", "limit 2000\nroot app\nbogus 2001",
            "limit 2000\nroot app\napp/src/file.rs 2000",
            "limit 2000\nroot app\nother/file.rs 2100",
            "limit 2000\nroot app\napp/src/file.rs 2100\napp/src/file.rs 2100",
        ):
            with self.subTest(text=text):
                with self.assertRaises(ValueError):
                    Budget.parse(text)

    def test_scanner_includes_hidden_source_but_not_build_outputs(self):
        (self.root / "app/Cargo.toml").write_text("[package]\n", encoding="utf-8")
        for name in ("app/src/normal.rs", "app/src/target/real.rs", "app/.internal/source.rs",
                     "app/target/generated.rs", "tmp/probe.rs", "third_party/copied.rs"):
            self.write(name, 1)
        names = {path.relative_to(self.root).as_posix() for path in source_files(self.root, ("app",))}
        self.assertEqual(names, {"app/src/normal.rs", "app/src/target/real.rs", "app/.internal/source.rs"})

    def test_missing_empty_and_unreadable_roots_fail(self):
        with self.assertRaises(ValueError):
            list(source_files(self.root, ("missing",)))
        self.assertIn("no first-party sources", check(self.root, self.policy())[0][0])
        with patch("scripts.architecture.budgets.os.walk", side_effect=PermissionError("unreadable")):
            with self.assertRaises(PermissionError):
                list(source_files(self.root, ("app",)))

    def test_symlinks_are_not_followed(self):
        self.write("outside.rs", 1)
        try:
            (self.root / "app/src/link.rs").symlink_to(self.root / "outside.rs")
        except OSError as error:
            self.skipTest(f"symlink unavailable: {error}")
        with self.assertRaises(ValueError):
            list(source_files(self.root, ("app",)))

    def test_symlinked_root_ancestor_is_rejected(self):
        try:
            (self.root / "redirect").symlink_to(self.root / "app", target_is_directory=True)
        except OSError as error:
            self.skipTest(f"symlink unavailable: {error}")
        with self.assertRaises(ValueError):
            list(source_files(self.root, ("redirect/src",)))

    def test_empty_existing_base_budget_is_not_bootstrap(self):
        with patch("scripts.check_architecture.Revision") as revision:
            revision.return_value.read.return_value = b""
            with self.assertRaises(ValueError):
                check_architecture.run(check_architecture.ROOT, base="HEAD")

    def test_base_rejects_limit_increase_and_scope_removal(self):
        self.write("app/src/view.rs", 1)
        self.assertIn("may not increase", check(self.root, self.policy(limit=2100), self.policy())[0][0])
        previous = Budget.parse("limit 2000\nroot app\nroot other")
        self.assertIn("may not be removed", check(self.root, self.policy(), previous)[0][0])

    def test_base_rejects_new_or_increased_exceptions(self):
        self.write("app/src/legacy.rs", 2100)
        current = self.policy("app/src/legacy.rs 2100")
        self.assertIn("new or increased", check(self.root, current, self.policy())[0][0])
        previous = self.policy("app/src/legacy.rs 2099")
        self.assertIn("new or increased", check(self.root, current, previous)[0][0])

    def test_base_actual_size_prevents_regrowth_under_stale_budget(self):
        self.write("app/src/legacy.rs", 2101)
        budget = self.policy("app/src/legacy.rs 2200")
        errors = check(self.root, budget, budget, lambda name: b"line\n" * 2100)[0]
        self.assertIn("2101 lines > 2100", errors[0])

    def test_base_that_already_shrank_below_limit_cannot_regrow(self):
        self.write("app/src/legacy.rs", 2100)
        budget = self.policy("app/src/legacy.rs 2200")
        errors = check(self.root, budget, budget, lambda name: b"line\n" * 1800)[0]
        self.assertIn("2100 lines > 2000", errors[0])

    def test_missing_base_source_is_an_error(self):
        self.write("app/src/legacy.rs", 2100)
        budget = self.policy("app/src/legacy.rs 2100")
        self.assertIn("no source in base", check(self.root, budget, budget, lambda name: None)[0][0])

    def test_invalid_base_is_not_treated_as_bootstrap(self):
        with patch("scripts.check_architecture.subprocess.run", side_effect=subprocess.CalledProcessError(128, "git")):
            with self.assertRaises(subprocess.CalledProcessError):
                check_architecture.Revision(self.root, "missing")

    def test_unreadable_base_tree_is_not_treated_as_missing_budget(self):
        revision = object.__new__(check_architecture.Revision)
        revision.root = self.root
        revision.commit = "a" * 40
        with patch("scripts.check_architecture.subprocess.run", side_effect=subprocess.CalledProcessError(128, "git")):
            with self.assertRaises(subprocess.CalledProcessError):
                revision.read("architecture/file-budgets.txt")

    def test_cli_fails_closed_when_configuration_cannot_be_read(self):
        with (patch.object(check_architecture, "ROOT", self.root),
              patch("sys.argv", ["check_architecture"]),
              patch("sys.stderr", new_callable=io.StringIO) as output):
            self.assertEqual(check_architecture.main(), 1)
            self.assertIn("could not complete", output.getvalue())


if __name__ == "__main__":
    unittest.main()
