import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import install
import share


class IntegrationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="spectacle-lan-bridge-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.helper = self.root / "home with spaces/.local/bin/lan-bridge-spectacle"
        self.desktop = self.root / "applications/test.desktop"

    def test_install_reinstall_uninstall_and_desktop_validation(self):
        install.install(self.helper, self.desktop)
        self.assertTrue(os.access(self.helper, os.X_OK))
        self.assertIn(f'Exec="{self.helper}" %f', self.desktop.read_text())
        self.assertIn("MimeType=image/png;", self.desktop.read_text())
        validator = shutil.which("desktop-file-validate")
        if validator:
            subprocess.run([validator, str(self.desktop)], check=True)
        install.install(self.helper, self.desktop)
        install.uninstall(self.helper, self.desktop)
        install.uninstall(self.helper, self.desktop)
        self.assertFalse(self.helper.exists())
        self.assertFalse(self.desktop.exists())

    def test_refuses_to_overwrite_or_remove_unrelated_files(self):
        self.helper.parent.mkdir(parents=True)
        self.helper.write_text("user work")
        for action in [install.install, install.uninstall]:
            with self.assertRaises(ValueError):
                action(self.helper, self.desktop)
            self.assertEqual(self.helper.read_text(), "user work")

    def test_refuses_symlinks(self):
        target = self.root / "user-file"
        target.write_text(install.HELPER_MARKER)
        self.helper.parent.mkdir(parents=True)
        self.helper.symlink_to(target)
        with self.assertRaises(ValueError):
            install.install(self.helper, self.desktop)
        self.assertEqual(target.read_text(), install.HELPER_MARKER)

    def test_exec_escapes_special_characters(self):
        entry = install.desktop_entry(Path('/home/a $b`c"d\\e%f/helper'))
        self.assertIn(r'\\$', entry)
        self.assertIn(r'\\`', entry)
        self.assertIn(r'\\"', entry)
        self.assertIn("%%f/helper", entry)
        with self.assertRaises(ValueError):
            install.desktop_entry(Path("/home/a=b/helper"))

    @patch.object(share, "notify")
    def test_calls_installed_binary_without_a_shell(self, notify):
        home = self.root / "home"
        binary = home / ".local/bin/lan-bridge"
        binary.parent.mkdir(parents=True)
        binary.touch()
        screenshot = self.root / "带 空格;$(echo bad).png"
        screenshot.write_bytes(b"PNG")
        with patch.object(share.Path, "home", return_value=home), patch.object(
            share.subprocess, "run",
            return_value=subprocess.CompletedProcess([], 0, "", ""),
        ) as run:
            self.assertEqual(share.main([str(screenshot)]), 0)
            self.assertEqual(
                run.call_args.args[0], [str(binary), "share", str(screenshot)]
            )
            self.assertNotIn("shell", run.call_args.kwargs)
            notify.assert_called_once()

    @patch.object(share, "notify")
    def test_missing_input_and_command_failure_do_not_report_success(self, notify):
        self.assertEqual(share.main([]), 2)
        self.assertEqual(share.main([str(self.root / "missing.png")]), 1)
        screenshot = self.root / "image.png"
        screenshot.touch()
        with patch.object(share.Path, "home", return_value=self.root), patch.object(
            share.shutil, "which", return_value="/fake/lan-bridge"
        ), patch.object(
            share.subprocess, "run",
            return_value=subprocess.CompletedProcess([], 1, "", "copy failed"),
        ):
            self.assertEqual(share.main([str(screenshot)]), 1)
            self.assertTrue(notify.call_args.kwargs["failed"])

    def test_adapter_copies_file_with_real_lan_bridge(self):
        binary = (
            Path(__file__).resolve().parent.parent
            / "lan-bridge-cli/target/debug/lan-bridge"
        )
        if not binary.exists():
            self.skipTest("先在 lan-bridge-cli 目录执行 cargo build，以运行实际文件分享测试")
        home = self.root / "home"
        destination = home / ".local/bin/lan-bridge"
        destination.parent.mkdir(parents=True)
        destination.symlink_to(binary)
        screenshot = self.root / "测试 截图.png"
        screenshot.write_bytes(b"\x89PNG\r\n\x1a\n")
        env = dict(os.environ, HOME=str(home), PATH="/usr/bin:/bin")
        env.pop("DATA_DIR", None)
        # Keep tests from displaying a desktop notification.
        with patch.object(share.Path, "home", return_value=home), patch.dict(
            os.environ, env, clear=True
        ), patch.object(share, "notify"):
            self.assertEqual(share.main([str(screenshot)]), 0)
        copied = home / ".local/share/lan-bridge/shared-files" / screenshot.name
        self.assertEqual(copied.read_bytes(), screenshot.read_bytes())


if __name__ == "__main__":
    unittest.main()
