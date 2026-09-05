import tempfile
import unittest
from pathlib import Path

from scripts.check_platform_cfg import count_platform_cfgs


class PlatformCfgTests(unittest.TestCase):
    def count(self, source: str) -> int:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "source.rs"
            path.write_text(source, encoding="utf-8")
            return count_platform_cfgs(path)

    def test_plain_target_os_is_counted(self) -> None:
        for target in ("macos", "linux", "freebsd"):
            self.assertEqual(self.count(f'#[cfg(target_os = "{target}")] fn example() {{}}'), 1)

    def test_nested_conditions_and_macros_are_counted(self) -> None:
        self.assertEqual(self.count('#[cfg(all(unix, not(target_os = "macos")))]'), 2)
        self.assertEqual(self.count('cfg!(any(windows, target_os = "linux"))'), 2)
        self.assertEqual(self.count('#[cfg_attr(target_os="macos", allow(dead_code))]'), 1)

    def test_features_are_not_platforms(self) -> None:
        self.assertEqual(self.count('#[cfg(feature = "gpui-shell")]'), 0)
        self.assertEqual(self.count('let windows = true;'), 0)
