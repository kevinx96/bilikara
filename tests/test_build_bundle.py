import unittest
from pathlib import Path
from unittest.mock import patch

import build_bundle


class BuildBundleTest(unittest.TestCase):
    def test_resolve_windows_binary_prefers_chocolatey_real_executable(self):
        shim = Path("/ProgramData/chocolatey/bin/ffmpeg.exe")
        real = Path("/ProgramData/chocolatey/lib/ffmpeg/tools/ffmpeg/bin/ffmpeg.exe")

        with patch("build_bundle.platform.system", return_value="Windows"), patch.object(
            Path,
            "exists",
            lambda self: self == real,
        ):
            resolved = build_bundle._resolve_windows_binary("ffmpeg", shim)

        self.assertEqual(resolved, real)


if __name__ == "__main__":
    unittest.main()
