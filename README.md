# 局域网传送

Rust 服务端，手机浏览器与电脑在同一局域网内交换文件和文本。

## 项目结构

- `lan-bridge-cli/`：Rust 服务、网页及 CLI 安装脚本。
- `spectacle-lan-bridge/`：Spectacle 截图导出集成。

以下命令均从仓库根目录执行。

## 安装

需要 Rust/Cargo；可选的 Spectacle 集成还需要 Python 3.10+，桌面通知使用 `notify-send`。

从仓库根目录执行，无需 `sudo`：

```bash
# 编译并安装 CLI 到 ~/.local/bin
./lan-bridge-cli/install.sh

# 可选：在 Spectacle 的 Export 菜单中添加“通过 LAN Bridge 分享”
python3 spectacle-lan-bridge/install.py
```

安装桌面集成后，完全退出并重新打开 Spectacle。如果终端提示找不到 `lan-bridge`，请将 `~/.local/bin` 加入 `PATH`，或使用完整路径 `~/.local/bin/lan-bridge`。

卸载 Spectacle 集成（不删除 CLI 或共享文件）：

```bash
python3 spectacle-lan-bridge/install.py --uninstall
```

## 运行

```bash
lan-bridge serve
```

也可不安装，直接从源码运行：

```bash
cargo run --manifest-path lan-bridge-cli/Cargo.toml --release -- serve
```

`serve` 在前台运行，启动成功后自动使用 Chrome 打开本次带访问密钥的分享网址。依次查找 `google-chrome-stable`、`google-chrome`；未安装 Chrome 或打开失败不影响服务运行，可手动打开终端中的链接，通过 `lan-bridge log` 查看失败记录。不会修改系统默认浏览器。终端只显示启动二维码、分享链接和使用提示，运行日志写入文件。无参数的 `lan-bridge` 仍等同于 `lan-bridge serve`。安装时不删除已有文件，也不清空日志。

```bash
lan-bridge share /path/to/file.pdf # 复制到共享目录，保留源文件
lan-bridge log                    # 输出当前日志，不持续跟踪
lan-bridge --help
```

默认路径不依赖当前工作目录：

```text
~/.local/bin/lan-bridge
~/.local/share/lan-bridge/
├── server.log
└── shared-files/
    └── .lan-bridge-state/latest-text.json
```

`share` 接收一个普通文件（最大 1 GiB），无需服务器正在运行；重名自动编号，不覆盖已有文件。运行中的页面可点击“刷新文件列表”查看通过命令分享的文件。它不会启动服务器或生成另一条分享链接。

文件列表支持 PNG、JPEG、WebP、GIF 图片预览，滚动到图片附近时才加载，点击缩略图可放大，按 Esc 或“关闭”退出。原下载按钮保留。预览使用访问密钥鉴权，不把密钥放在图片地址中；不展示 SVG，并校验图片文件头。缩略图是下载原图后缩小显示，不生成服务端缩略图；最多同时加载两张，单张预览上限 20 MiB，超限或损坏时仍可下载查看。

页面修改需要重新编译并重启服务才能生效。可用 Node.js 18+ 执行页面逻辑测试：`node --test lan-bridge-cli/tests/page.test.cjs`。

启动后终端直接显示二维码；手机连同一 Wi-Fi 后扫描终端二维码即可打开页面。电脑也可打开终端打印的完整链接。文件默认保存在 `~/.local/share/lan-bridge/shared-files`，这个目录本身就是共享文件夹：在电脑上直接放进去（或删掉）的文件，点击页面“刷新文件列表”后即可看到，手机和其他电脑都能下载。网页上传成功后会通知已连接页面更新列表。手机上传的文件以原文件名保存，重名会自动加 `(1)`；列表按修改时间倒序。可用 `PORT=8080` 指定端口，通过环境变量 `LAN_INTERFACE` 指定优先使用的网卡，`LAN_IP=192.168.1.95` 直接指定二维码里的电脑地址，`DATA_DIR=/path/to/files` 单独指定共享目录（`serve` 和 `share` 需使用相同设置；日志位置不变）。

旧的项目内 `./shared-files` 不会自动迁移或删除；可以手动迁移文件，或继续通过 `DATA_DIR` 指向旧目录。

## Spectacle 截图分享

独立子项目 [`spectacle-lan-bridge/`](spectacle-lan-bridge/README.md) 可在 Spectacle 的 Export 菜单添加“通过 LAN Bridge 分享”，点击后调用 `lan-bridge share`。安装方式见子项目文档。

链接中含启动时生成的随机访问密钥，请勿公开。服务监听所有网卡；使用后按 Ctrl+C 关闭。单个文件上限 1 GiB；文本上限 128 KiB。

## 文本分享与更新通知

文本由页面手动发送，只保留最新一条。文本和版本号保存在共享目录的 `.lan-bridge-state/latest-text.json`，重启后恢复。写盘成功后才更新内存并通知页面；写入失败不覆盖原状态。状态目录不进入文件列表，下载接口不能访问其中文件；它仍是本机上的明文敏感数据，请保护共享目录及备份，不要把整个目录用其他服务公开。

状态文件不存在时从空文本开始；文件损坏或无法读取时启动失败，不会静默清空已有数据。

HTTP 负责文本读写、文件列表、上传和下载；WebSocket 仅推送变更通知，页面收到通知后请求最新数据，没有定时轮询。连接建立时加载最新状态。断线不自动重连，可点击“重新连接”；服务重启会更换访问密钥，需要重新扫码。也可手动刷新文本或文件列表。不监听共享目录的外部变动。

文本接口为 `/api/text`，通知接口为 `/api/events`；旧的剪贴板命名接口已移除。

## 故障日志

运行日志只写入 `~/.local/share/lan-bridge/server.log`，不输出到终端。每次 `serve` 启动时清空该文件；单次运行上限为 10 MiB，下一次写入将超过上限时清空并继续写，不生成备份、不保留历史。`lan-bridge log` 查看当前内容，不会清空日志；尚未启动过服务时会提示没有日志。若要保留故障记录，请在下次启动或达到上限前自行复制。

不提供详细诊断开关。记录启动、上传结果、下载开始、文本保存及请求失败；请求日志包含时间、级别、请求 ID、操作、设备 IP、状态码和响应准备耗时。正常列表及文本读取不输出日志。下载开始或响应准备成功不代表下载完成；读取文件流失败会单独记录。

日志不包含文件名、分享文本正文、访问密钥或完整请求链接。存储错误仅记录错误类别及系统错误码，避免文件路径或内容泄漏。失败响应的 `x-request-id` 可用于定位对应日志；请求失败不由服务端自动重试。注意：启动时供扫码使用的链接和二维码仍包含密钥，不要公开终端截图或完整输出。
