import re
import shutil
import subprocess
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


class WindowsResourceTests(unittest.TestCase):
    def resource(self, gpui):
        compiler = shutil.which("cc") or shutil.which("clang") or shutil.which("gcc")
        self.assertIsNotNone(compiler, "a C preprocessor is required for resource tests")
        command = [compiler, "-E", "-P", "-x", "c"]
        if gpui:
            command.append("-DNEBULA_GPUI_MANIFEST")
        command.append(str(ROOT / "nebula_app/windows/nebula.rc"))
        return subprocess.check_output(command, text=True, encoding="utf-8")

    def test_gpui_keeps_product_resources_without_a_second_manifest(self):
        resource = self.resource(True)
        self.assertIsNone(re.search(r"^1\s+24\s+", resource, re.MULTILINE))
        self.assertIn('ICON "nebula.ico"', resource)
        self.assertIn('"ProductName", "Pebrel"', resource)
        self.assertIn('"FileDescription", "Pebrel"', resource)
        self.assertIn('"OriginalFilename", "nebula.exe"', resource)
        self.assertIn("1 VERSIONINFO", resource)

    def test_legacy_shell_still_embeds_its_manifest(self):
        resource = self.resource(False)
        self.assertEqual(len(re.findall(r"^1\s+24\s+nebula\.manifest$", resource, re.MULTILINE)), 1)

    def test_build_script_selects_the_resource_contract(self):
        source = (ROOT / "nebula_app/build.rs").read_text(encoding="utf-8")
        self.assertRegex(source, r'if cfg!\(feature = "gpui-shell"\)\s*\{\s*&\["NEBULA_GPUI_MANIFEST"\]')
        self.assertIn('embed_resource::compile("./windows/nebula.rc", defines)', source)
        self.assertIn("cargo:rerun-if-changed=windows/nebula.manifest", source)
