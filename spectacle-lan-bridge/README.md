# Spectacle → LAN Bridge

独立的用户级桌面集成子项目，不修改 LAN Bridge 服务或 Spectacle 源码，不需要额外 Python 包。

在 Spectacle 的 **Export / 导出** 菜单中增加 **通过 LAN Bridge 分享**。点击后执行 `lan-bridge share <截图路径>`，复制图片到共享目录，不删除源图片、不启动服务器。

## 原理与边界

Spectacle 6.7.4 的 Export 菜单通过 `KApplicationTrader::queryByMimeType("image/png")` 获取应用条目；本项目注册一个支持 PNG 的 `.desktop` 文件。未保存的截图由 Spectacle 自行保存后传入适配器，无需脚本重新截屏。

这不是 Purpose 的二级“Share”插件，菜单项位于 Export 的应用列表。该应用也可能出现在其他程序的 PNG“打开方式”列表中，但不会更改默认图片应用。`NoDisplay=true` 避免在启动器中显示一个无法独立使用的应用。

依据：[Spectacle 6.7.4 ExportMenu.cpp](https://github.com/KDE/spectacle/blob/v6.7.4/src/Gui/ExportMenu.cpp)、[Desktop Entry Exec 规范](https://specifications.freedesktop.org/desktop-entry-spec/latest/exec-variables.html)。

## 安装

需要 Python 3.10+、已支持 `share` 命令的 LAN Bridge；桌面通知使用可选的 `notify-send`。

在仓库根目录执行以下命令，先更新 `~/.local/bin/lan-bridge`，再安装桌面集成：

```bash
./lan-bridge-cli/install.sh
python3 spectacle-lan-bridge/install.py
```

安装脚本只写入：

- `~/.local/bin/lan-bridge-spectacle`
- `~/.local/share/applications/io.github.lanbridge.Spectacle.desktop`
  （应用条目遵循 `XDG_DATA_HOME`）

脚本会刷新 MIME/KDE 应用缓存。不修改 `mimeapps.list` 或 Spectacle 配置，不申请 root 权限；可以重复运行。遇到同路径非本项目文件会拒绝覆盖。

**完全退出并重新打开 Spectacle**，截图后打开 Export，点击“通过 LAN Bridge 分享”。成功或失败会显示桌面通知。若未出现菜单项，可运行 `kbuildsycoca6` 后重新打开 Spectacle。

适配器优先调用 `~/.local/bin/lan-bridge`，因此不依赖桌面会话的 PATH 包含 `~/.local/bin`；找不到时才从 PATH 搜索。

默认复制到 `~/.local/share/lan-bridge/shared-files`，实际路径由 LAN Bridge 决定。若自定义 `DATA_DIR`，须确保桌面启动的适配器和服务器继承相同环境，单独在终端设置不会自动影响 Spectacle。

接收方需要运行 `lan-bridge serve` 并打开分享页面。命令复制文件不触发服务端通知，已打开的页面需点击“刷新文件列表”。

## 卸载

```bash
python3 spectacle-lan-bridge/install.py --uninstall
```

只删除本项目的两个安装文件，不删除截图、共享文件、LAN Bridge 或其他应用配置。

## 测试

在父目录运行：

```bash
TMPDIR=/tmp/opencode python3 -m unittest discover -s spectacle-lan-bridge -v
```

`TMPDIR` 可替换成自己的临时目录。测试在隔离目录安装和复制文件，不修改实际桌面配置；若 `lan-bridge-cli/target/debug/lan-bridge` 存在，会验证真实的 `lan-bridge share` 调用。可先执行 `cargo build --manifest-path lan-bridge-cli/Cargo.toml` 构建该二进制。
