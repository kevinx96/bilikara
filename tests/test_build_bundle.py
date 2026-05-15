import unittest
from pathlib import Path
from tempfile import TemporaryDirectory
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

    def test_resolve_windows_binary_finds_chocolatey_ffprobe_in_ffmpeg_package(self):
        shim = Path("/ProgramData/chocolatey/bin/ffprobe.exe")
        real = Path("/ProgramData/chocolatey/lib/ffmpeg/tools/ffmpeg/bin/ffprobe.exe")

        with patch("build_bundle.platform.system", return_value="Windows"), patch.object(
            Path,
            "exists",
            lambda self: self == real,
        ):
            resolved = build_bundle._resolve_windows_binary("ffprobe", shim)

        self.assertEqual(resolved, real)

    def test_resolve_windows_binary_rejects_unresolved_chocolatey_shim(self):
        shim = Path("/ProgramData/chocolatey/bin/ffprobe.exe")

        with patch("build_bundle.platform.system", return_value="Windows"), patch.object(
            Path,
            "exists",
            lambda self: False,
        ):
            resolved = build_bundle._resolve_windows_binary("ffprobe", shim)

        self.assertIsNone(resolved)

    def test_resolve_bundle_binary_path_rejects_unresolved_windows_shim(self):
        shim = Path("/ProgramData/chocolatey/bin/ffprobe.exe")

        with patch("build_bundle.platform.system", return_value="Windows"), patch(
            "build_bundle.shutil.which",
            return_value=str(shim),
        ), patch.object(
            Path,
            "exists",
            lambda self: False,
        ):
            resolved = build_bundle._resolve_bundle_binary_path("ffprobe")

        self.assertIsNone(resolved)

    def test_resolve_ffprobe_from_ffmpeg_sibling_when_not_on_path(self):
        ffmpeg = Path("/tools/ffmpeg")
        ffprobe = Path("/tools/ffprobe")

        def fake_which(binary_name: str):
            return str(ffmpeg) if binary_name == "ffmpeg" else None

        with patch("build_bundle.platform.system", return_value="Linux"), patch(
            "build_bundle.shutil.which",
            side_effect=fake_which,
        ), patch.object(
            Path,
            "exists",
            lambda self: self == ffprobe,
        ):
            resolved = build_bundle._resolve_bundle_binary_path("ffprobe")

        self.assertEqual(resolved, ffprobe)

    def test_bundled_binary_args_allows_missing_optional_ffprobe(self):
        ffmpeg = Path("/usr/bin/ffmpeg")
        data_separator = ";" if build_bundle.platform.system() == "Windows" else ":"

        def fake_resolve(binary_name: str):
            return ffmpeg if binary_name == "ffmpeg" else None

        with patch("build_bundle._resolve_bundle_binary_path", side_effect=fake_resolve):
            args = build_bundle._bundled_binary_args(data_separator)

        self.assertEqual(args, ["--add-binary", f"{ffmpeg.resolve()}{data_separator}vendor"])

    def test_bundled_binary_args_includes_resolved_optional_ffprobe(self):
        ffmpeg = Path("/usr/bin/ffmpeg")
        ffprobe = Path("/usr/bin/ffprobe")
        data_separator = ";" if build_bundle.platform.system() == "Windows" else ":"

        def fake_resolve(binary_name: str):
            return {"ffmpeg": ffmpeg, "ffprobe": ffprobe}.get(binary_name)

        with patch("build_bundle._resolve_bundle_binary_path", side_effect=fake_resolve):
            args = build_bundle._bundled_binary_args(data_separator)

        self.assertEqual(
            args,
            [
                "--add-binary",
                f"{ffmpeg.resolve()}{data_separator}vendor",
                "--add-binary",
                f"{ffprobe.resolve()}{data_separator}vendor",
            ],
        )

    def test_python_https_args_includes_hidden_imports(self):
        with patch("build_bundle.platform.system", return_value="Linux"):
            args = build_bundle._python_https_args(":")

        for module_name in build_bundle.PYTHON_HTTPS_HIDDEN_IMPORTS:
            self.assertIn("--hidden-import", args)
            self.assertIn(module_name, args)

    def test_python_https_binary_paths_collects_windows_openssl_dlls(self):
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            bin_dir = root / "Library" / "bin"
            bin_dir.mkdir(parents=True)
            ssl_dll = bin_dir / "libssl-3-x64.dll"
            crypto_dll = bin_dir / "libcrypto-3-x64.dll"
            ignored_pdb = bin_dir / "libssl-3-x64.pdb"
            ssl_dll.write_text("", encoding="utf-8")
            crypto_dll.write_text("", encoding="utf-8")
            ignored_pdb.write_text("", encoding="utf-8")

            with patch("build_bundle.platform.system", return_value="Windows"), patch.object(
                build_bundle.sys,
                "prefix",
                str(root),
            ), patch.object(build_bundle.sys, "base_prefix", str(root)), patch.object(
                build_bundle.sys,
                "exec_prefix",
                str(root),
            ), patch.object(build_bundle.sys, "base_exec_prefix", str(root)):
                paths = build_bundle._python_https_binary_paths()

        self.assertEqual({path.name for path in paths}, {"libssl-3-x64.dll", "libcrypto-3-x64.dll"})


if __name__ == "__main__":
    unittest.main()
