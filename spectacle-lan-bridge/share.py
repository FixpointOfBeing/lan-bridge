#!/usr/bin/env python3
# lan-bridge-spectacle integration
"""Spectacle export adapter. No shell interpolation and no server lifecycle."""

from pathlib import Path
import shutil
import subprocess
import sys


def notify(message: str, *, failed: bool = False) -> None:
    executable = shutil.which("notify-send")
    if not executable:
        return
    try:
        subprocess.run(
            [
                executable,
                "--app-name=LAN Bridge",
                "--icon=document-share",
                "--urgency=" + ("critical" if failed else "normal"),
                "LAN Bridge",
                message,
            ],
            check=False,
            timeout=5,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except (OSError, subprocess.TimeoutExpired):
        pass  # Notification failure must not change the result of sharing.


def main(args: list[str] | None = None) -> int:
    args = sys.argv[1:] if args is None else args
    if len(args) != 1:
        message = "需要一个截图文件路径。请从 Spectacle 的 Export 菜单分享截图。"
        print(message, file=sys.stderr)
        notify(message, failed=True)
        return 2

    source = Path(args[0]).absolute()
    if not source.is_file():
        message = "截图文件不存在或不是普通文件。请重新截图后再试。"
        print(message, file=sys.stderr)
        notify(message, failed=True)
        return 1

    local_binary = Path.home() / ".local/bin/lan-bridge"
    executable = str(local_binary) if local_binary.is_file() else shutil.which("lan-bridge")
    if not executable:
        message = "找不到 lan-bridge，请先将它安装到 ~/.local/bin。"
        print(message, file=sys.stderr)
        notify(message, failed=True)
        return 1

    try:
        result = subprocess.run(
            [executable, "share", str(source)],
            check=False,
            capture_output=True,
            text=True,
            errors="replace",
        )
    except OSError as error:
        print(f"无法执行 lan-bridge：{error}", file=sys.stderr)
        notify("无法执行 lan-bridge，请检查安装及执行权限。", failed=True)
        return 1

    if result.returncode:
        print(result.stderr or "lan-bridge share 执行失败", file=sys.stderr)
        notify(
            "分享失败，请在终端运行 lan-bridge share <图片路径> 查看原因。",
            failed=True,
        )
        return 1

    notify("截图已复制到共享目录。请在接收页面点击“刷新文件列表”。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
