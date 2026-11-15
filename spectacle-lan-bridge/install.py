#!/usr/bin/env python3
"""Install only user-owned desktop integration; never change MIME defaults."""

import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys

APP_ID = "io.github.lanbridge.Spectacle.desktop"
HELPER_NAME = "lan-bridge-spectacle"
HELPER_MARKER = "# lan-bridge-spectacle integration"
DESKTOP_MARKER = "X-LANBridge-Spectacle=true"


def desktop_entry(helper: Path) -> str:
    value = str(helper)
    if any(character in value for character in "\r\n\t="):
        raise ValueError("安装路径不能包含换行、制表符或等号")
    # Desktop Entry has two escaping layers: Exec quoting, then string escaping.
    quoted = '"' + "".join(
        "\\" + character if character in '"`$\\' else character
        for character in value.replace("%", "%%")
    ) + '"'
    executable = quoted.replace("\\", "\\\\")
    return f"""[Desktop Entry]
Type=Application
Name=Share via LAN Bridge
Name[zh_CN]=通过 LAN Bridge 分享
Comment=Copy a screenshot to the LAN Bridge shared folder
Comment[zh_CN]=将截图复制到 LAN Bridge 共享目录
Exec={executable} %f
Icon=document-share
Terminal=false
NoDisplay=true
StartupNotify=false
MimeType=image/png;
Categories=Network;
{DESKTOP_MARKER}
"""


def paths() -> tuple[Path, Path]:
    home = Path.home()
    data = Path(os.environ.get("XDG_DATA_HOME") or home / ".local/share")
    if not data.is_absolute():
        raise ValueError("XDG_DATA_HOME 必须是绝对路径")
    return home / ".local/bin" / HELPER_NAME, data / "applications" / APP_ID


def check_owned(path: Path, marker: str) -> None:
    if path.is_symlink() or (path.exists() and marker not in path.read_text()):
        raise ValueError(f"拒绝覆盖或删除非本项目文件：{path}")


def install(helper: Path, desktop: Path) -> None:
    check_owned(helper, HELPER_MARKER)
    check_owned(desktop, DESKTOP_MARKER)
    content = desktop_entry(helper)
    helper.parent.mkdir(parents=True, exist_ok=True)
    desktop.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(Path(__file__).with_name("share.py"), helper)
    helper.chmod(0o755)
    desktop.write_text(content, encoding="utf-8")


def uninstall(helper: Path, desktop: Path) -> None:
    # Check both before touching either; do not delete unrelated user files.
    check_owned(helper, HELPER_MARKER)
    check_owned(desktop, DESKTOP_MARKER)
    helper.unlink(missing_ok=True)
    desktop.unlink(missing_ok=True)


def refresh_cache(applications: Path) -> None:
    for name, args in [
        ("update-desktop-database", [str(applications)]),
        ("kbuildsycoca6", []),
    ]:
        executable = shutil.which(name)
        if executable:
            result = subprocess.run(
                [executable, *args], check=False, capture_output=True, text=True
            )
            if result.returncode:
                print(f"警告：{name} 失败，请手动刷新 KDE 应用缓存。", file=sys.stderr)
        else:
            print(f"提示：未找到 {name}，可能需要手动刷新应用缓存。", file=sys.stderr)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--uninstall", action="store_true", help="卸载此导出菜单项")
    args = parser.parse_args()
    try:
        helper, desktop = paths()
        if args.uninstall:
            uninstall(helper, desktop)
            print("已卸载 Spectacle 的 LAN Bridge 导出入口。")
        else:
            install(helper, desktop)
            print(f"已安装：{desktop}")
        refresh_cache(desktop.parent)
        print("请完全退出并重新打开 Spectacle。")
        return 0
    except (OSError, ValueError) as error:
        print(f"操作失败：{error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
