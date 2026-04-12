import os
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

from bilikara import config


class ConfigPathTest(unittest.TestCase):
    def test_frozen_build_defaults_to_runtime_within_app_folder(self):
        with TemporaryDirectory() as temp_dir:
            fake_executable = Path(temp_dir) / "bilikara.exe"
            fake_executable.touch()
            expected_home = fake_executable.resolve().parent / "runtime"
            with patch.dict(os.environ, {}, clear=True):
                with patch.object(config.sys, "frozen", True, create=True):
                    with patch.object(config.sys, "executable", str(fake_executable)):
                        self.assertEqual(config._default_app_home(), expected_home)


if __name__ == "__main__":
    unittest.main()
