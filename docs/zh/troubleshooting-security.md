# 故障排查、常见问题（FAQ）与安全

本指南介绍运营者最常遇到的问题、简短的常见问题（FAQ），以及听众音频流和远程控制面板的安全模型。设置项名称、默认值和按钮文字均取自源代码。未在真实硬件上运行过的行为标注为 **（未验证）**。完整列表见文末。

相关页面：[网络、玩家与主持人远程面板](./network-remote.md)、[配置参考](./configuration.md)、[音频设备、录制、配置方案与更新](./audio-session-tools.md)、[HTTPS（安全连接）](./https.md)。

## 目录

- [故障排查](#troubleshooting)
- [常见问题（FAQ）](#faq)
- [安全模型](#security-model)
- [真实硬件上未验证](#not-verified-on-real-hardware)

<a id="troubleshooting"></a>

## 故障排查

### 初步检查

先查看 **Network Server**（网络服务器）面板。状态以大写字母显示：

| 状态 | 其下方的行 | 含义 |
|---|---|---|
| `Live` | `Broadcasting to N listeners` | 服务器正在运行。 |
| `Starting` | `Binding the listen socket` | 正在启动。 |
| `Error` | 错误文本，截断为 34 个字符 | 服务器无法启动。完整文本见日志。 |
| `Off Air` | `Server stopped` | 服务器未运行。 |

**Listen URL**（监听地址）下方可能会出现 `Local ...` 行、`Internet ...` 行以及琥珀色提示（`VPN active ...`、`Firewall: ...`）。端口和绑定地址在 **CONFIGURATION**（配置）中编辑，且仅在服务器停止时可以编辑。

**日志在哪里？** 默认情况下，日志仅输出到终端，Windows 发行版构建没有控制台窗口。要保留日志，需在 `[logging]` 中设置 `file_logging = true`。此时 Bardwave 还会在数据目录中写入 `bardwave.log`；使用配置方案（profile）时，该文件位于根数据目录，所有配置方案共用。`level` 默认为 `info`（可选值为 `trace`、`debug`、`info`、`warn` 或 `error`）。当 `RUST_LOG` 环境变量包含有效的过滤器时，它会替代 `level`。日志从不包含主持人（GM）令牌，但访问密钥可能出现在日志中（参见[密钥轮换](#rotating-keys)）。

```toml
[logging]
level = "info"
file_logging = true   # writes bardwave.log in the data folder
```

### 无声音

检查 **AUDIO OUTPUT**（音频输出）部分中 **Output**（主输出）下拉列表下方的状态行。它显示为 `Active: <device> - 48000 Hz, 2 ch`，或 `Error: ...`。

| 现象 | 可能原因 | 解决方法 |
|---|---|---|
| 声音从错误的声卡播放。状态行中有类似 `output device "X" not found; using the system default` 的提示 | `[audio] output_device` 指定的设备未找到。Bardwave 会回退到系统默认设备。匹配规则为：先精确匹配，再忽略大小写和首尾空格进行匹配。 | 在下拉列表中重新选择设备，或删除 `output_device`。接入设备后，点击 **Refresh devices**（刷新设备列表）。 |
| `Error: no output device opened (network only)` | 无法打开任何声卡。玩家仍能收到音频流。 | 检查系统输出设备，然后点击 **Refresh devices**。 |
| 没有声音，也没有提示 | 主音量为零（静音时，“MASTER OUTPUT”（主输出）条带上会显示 **Unmute**（取消静音））。或者某个总线（bus）推子为零：**Music**（音乐）、**Ambience**（氛围音）或 **Sfx**（音效），位于“MIX”（混音）部分。或者 **Duck**（闪避）处于锁定状态（按钮显示 **Ducked**），会降低音乐和氛围音。 | 点击 **Unmute**，调高主音量和推子，再次点击 **Ducked** 解除锁定。 |
| 某个声音行显示为琥珀色并带有警告 | 其文件缺失。 | 参见[缺失文件](#missing-files)。 |
| 玩家听不到声音，但控制台可以播放 | 他们的链接中没有密钥，或者他们尚未点击 **Establish Link**（建立连接）。听众页面需要点按一次才能开始播放音频。 | 发送带有 `?k=<key>` 的完整链接（点击 **Copy Link**（复制链接）获取），并请他们点击 **Establish Link**。 |
| 音频流中没有耳机试听（preview） | 试听内容按设计从不发送给玩家。 | 只在本机收听试听内容。 |
| 试听从主输出播放 | `[audio] preview_device` 未设置，或未找到该设备（此时试听会从主输出播放，并附带提示）。 | 将其设置为耳机设备名称。 |

`master_volume` 的初始值为 `0.8`，启动时从 `config.toml` 读取。控制台的主推子和总线推子不会写回该文件。要更改启动音量，请编辑 `master_volume`（从 `0.0` 到 `1.0` 的数字）。

```toml
[audio]
output_device = "USB Headphones"    # main mix; omit for the system default
preview_device = "Built-in Output"  # previews only
master_volume = 0.8
```

### 爆音与断续

| 现象 | 可能原因 | 解决方法 |
|---|---|---|
| 控制台有咔嗒声或爆音 | 声卡缓冲区对此驱动程序而言过小。 | 增大 `buffer_size`，例如设为 `1920`，然后重新启动 Bardwave（未验证）。 |
| 控制台忽略某个值 | `buffer_size` 超出 `0` 或 `64`-`8192` 的范围。日志会发出警告，并使用 `480` 帧（而不是文件中默认的 `960`）。 | 使用 `0`（驱动程序默认值）或范围内的值。 |
| `sample_rate` 或 `channels` 被忽略 | 超出范围（`8000`-`192000` Hz，`1`-`8` 个声道）。日志会发出警告，并使用 `48000` Hz 立体声。 | 使用范围内的值。 |
| 状态行显示设备格式不同 | 该声卡不支持 48 kHz 立体声模式。混音会为该设备重采样或转换。玩家的音频流不受影响。 | 通常不需要处理。如果音质不佳，可选择其他设备。 |
| 听众在 Wi-Fi 下听到断续 | 网络丢包。听众页面会依次间隔 1、2、4、8 秒重新连接，之后间隔为 15 秒；页面会显示 `Reconnecting #N`，以及链路质量（延迟、缓冲、10 秒内的丢包）和 `Weak network` 提示。当某个客户端的发送缓冲区已满（100 个数据包，约 2 秒）时，服务器会丢弃发给该客户端的帧。 | 靠近路由器，或改用网线。 |

**AUDIO OUTPUT** 部分显示当前生效的值，例如 `buffer: 960 frames (20.0 ms) - 48000 Hz - 2 ch`。这些值在该处为只读。

```toml
[audio]
buffer_size = 1920   # frames per channel; default 960 (20 ms at 48 kHz); 0 = driver default
```

> **注意：** 缓冲区越大，控制台输出的延迟越高。该配置项仅在启动时读取一次，因此每次只修改一个值，并重新启动 Bardwave。无论 `buffer_size` 为何值，发送给玩家的数据包始终为 960 帧（20 ms）的 Opus 音频。

### 玩家连接故障

| 现象 | 可能原因 | 解决方法 |
|---|---|---|
| `Off Air` / `Server stopped` | 服务器未运行。 | 点击 **Start Server**（启动服务器）。 |
| `Error` 状态 | 无法绑定监听套接字，通常是因为端口已被占用。 | 在服务器停止时，于 CONFIGURATION 中修改 **Port**（端口），然后点击 **Retry Server**（重试服务器）。 |
| CONFIGURATION 下显示 `Port must be 1–65535` 或 `Bind must be an IP address` | 该值无效。仍沿用上一个有效值。“Bind Address”（绑定地址）只接受 IP 地址，不接受主机名。 | 输入有效值。 |
| 页面可以打开，但显示 `Access Denied`、`This link has no access key` | 链接缺少访问密钥。 | 点击 **Copy Link** 并发送该链接。 |
| 页面可以打开，但显示 `Access Denied`、`This link is no longer valid: the GM issued a new access key` | 链接分享后，密钥已更换（关闭码 `4001`）。 | 发送新链接。 |
| 页面打开后处于空闲状态 | 页面在等待点按。 | 点击 **Establish Link**。 |
| `Link Error`、`Connection failed. Check the server.` | 无法建立连接。 | 检查服务器是否正在运行，以及地址是否正确。 |
| 页面可以打开，但样式缺失，图标不显示 | 听众页面在玩家的浏览器中从 `cdn.tailwindcss.com`、`code.iconify.design` 和 `fonts.googleapis.com` 加载 Tailwind、图标和字体。无法联网的设备无法获取这些资源（此时页面的显示效果未验证）。 | 让该设备接入互联网，或使用可以联网的设备。 |
| 只有本机可以连接 | `host` 为 `127.0.0.1` 或 `::1`。绑定到环回地址时不会转发。 | 在服务器停止时，将 **Bind Address** 设为 `0.0.0.0`（`host = "0.0.0.0"`）。 |
| macOS 上，局域网（LAN）内的任何人都无法连接 | 本地网络提示选择了“不允许”。 | “系统设置 > 隐私与安全性 > 本地网络”（在 Mac 上未验证）。 |
| 局域网可用，互联网上的玩家无法连接 | UPnP、双重 NAT、VPN 或防火墙。 | 参见下面的相关章节。 |

```toml
[server]
host = "0.0.0.0"   # every IPv4 network of this PC
port = 8080
```

<a id="wrong-lan-address"></a>

### 局域网地址错误

Bardwave 会收集本机拥有的所有地址并为其评分。私有局域网地址段优先（`192.168.x.x`，其次是 `10.x.x.x`，然后是 `172.16.0.0/12`，Docker 和 WSL 通常位于此段）。其次是公网 IP，最后才考虑运营商级 NAT（CGNAT）地址（`100.64.0.0/10`，Tailscale 使用此段）。基准测试地址段（`198.18.0.0/15`，代理 TUN 适配器使用）、链路本地地址、环回地址和保留地址段从不选用。在 macOS 和 Linux 上，使用虚拟适配器名称的适配器（例如 `utun`、`docker`、`veth`、`br-`、`tun`、`tap`、`wg`、`tailscale`、`zt`）会被跳过。在 Windows 上，隧道和环回适配器会被跳过。通配绑定只会公布与其地址族相同的地址。如果没有符合条件的地址，链接会使用环回地址，玩家无法打开该链接。

| 现象 | 可能原因 | 解决方法 |
|---|---|---|
| 链接显示 `127.0.0.1` | 未找到可用的局域网地址，或 `host` 为环回地址。 | 检查网络连接。或者在 **Bind Address** 中输入本机的局域网地址。 |
| 链接显示的不是局域网地址（例如来自 Docker 或 WSL 的 `172.x.x.x`，来自 Tailscale 的 `100.x.x.x`） | 这是找到的最佳地址。 | 在服务器停止时，将玩家应使用的地址输入 **Bind Address**。明确指定的地址会按原样公布。绑定到该地址后，UPnP 请求也会限定在该网络接口上。 |
| 链接地址正确，但玩家仍然无法连接 | 防火墙、VPN，或设备位于不同网络（例如访客 Wi-Fi）。 | 参见[防火墙](#firewall)和[VPN 与代理工具](#vpn-and-proxy-tools)。 |

“Listen URL”下方的 `Local ...` 行不是局域网地址。它始终是 `127.0.0.1` 加端口，用于在检测结果有误时也能在本机打开页面。检测不会发送任何数据包：它只是询问系统，路由时会使用哪个地址。随时可以手动构造链接：`http://<LAN address of this PC>:<port>/?k=<key>`。

### UPnP 失败与双重 NAT

`[server] upnp = true` 为默认值。Bardwave 会在后台请求路由器转发 TCP 端口（仅限 UPnP，仅限 IPv4）。局域网链接会立即显示，服务器从不等待路由器响应。当 `upnp = false`，或 `host` 为环回地址或 IPv6 地址时，不会显示 `Internet` 行。

| 控制台中的“Internet”行 | 含义 | 解决方法 |
|---|---|---|
| `Internet: asking the router…` | 第一次尝试正在进行。 | 等待几秒。 |
| `Internet: no UPnP router` | 没有路由器响应，或路由器拒绝了请求。Bardwave 每 30 秒重试一次。将鼠标悬停在该行上可查看最后一次失败的原因。 | 在路由器中启用 UPnP。也可以手动设置端口转发（未验证）。 |
| `Internet: blocked by a second NAT (<address>)` | 路由器报告的地址不是公网 IP（私有地址、运营商级 NAT 等），因此其前方还有另一层 NAT。 | 在上游设备上设置端口转发，或向运营商申请公网 IP。 |
| `Internet http://<address>:<port>`（HTTPS 已开启时为 `https://`） | 映射为公网可访问。点击该行即可复制。 | 用手机的移动数据测试。在自己的网络内部打开，在不支持 NAT 环回的路由器上可能会失败。 |

请求的租约为 2 小时。续期时间为路由器授予租约的一半（至少每 60 秒一次）。永久租约每 30 分钟检查一次。如果路由器返回错误 `725`（仅适用于永久租约），Bardwave 会以永久租约重新请求。如果路由器返回 `718`（该端口已转发给其他主机），Bardwave 会尝试其他外部端口，最多尝试四次，因此互联网链接所用的端口可能与设置的端口不同。映射会在点击 **Stop Server**（停止服务器）时移除。这些路由器响应仅在代码和单元测试中处理（在真实路由器上未验证）。

```toml
[server]
upnp = false   # LAN play only; no router requests
```

<a id="vpn-and-proxy-tools"></a>

### VPN 与代理工具

全隧道 VPN 会将发给互联网玩家的回复送入隧道，因此他们无法连接。当默认路由未通过所选的局域网地址发出时，控制台会显示琥珀色提示：

> `VPN active (<address>): internet players can't connect until it is off`

局域网联机仍可正常使用。在邀请互联网玩家之前，先关闭 VPN，或将 Bardwave 排除在 VPN 之外（在真实 VPN 客户端上未验证）。TUN 模式代理（Clash、xray、sing-box）使用基准测试地址段，Bardwave 从不公布该地址段。局域网链接仍然正确，但很可能会出现相同的提示（在这些工具上未验证）。参见[局域网地址错误](#wrong-lan-address)。

<a id="firewall"></a>

### 防火墙

| 平台 | 发生的情况 | 解决方法 |
|---|---|---|
| Windows | 服务器启动时，Bardwave 会添加一条名为 `Bardwave TCP <port>` 的 TCP 入站规则，网络配置文件为 `any`。每次启动时都会替换该规则。创建该规则需要管理员权限。安装程序可以为 TCP `8080` 添加等效规则（专用网络和公用网络）。 | 没有管理员权限时，控制台会显示 `Firewall: port <port> not opened - allow Bardwave for public networks or run as admin`。Windows 可能会在首次监听时询问：为 Bardwave 允许 **专用网络和公用网络**。 |
| macOS | Bardwave 不添加防火墙规则。应用包声明了本地网络使用。 | 在系统提示时允许传入连接和本地网络访问（在 Mac 上未验证）。 |
| Linux | Bardwave 不添加防火墙规则。 | 如果防火墙已启用，自行开放监听 TCP 端口（默认为 8080），例如使用 `ufw` 或 `firewalld`（未验证）。 |

> **警告：** 不要为了测试而关闭防火墙。只开放监听端口。

### YouTube 下载失败

声音库中的 **YouTube** 按钮会打开 **Add From YouTube**（从 YouTube 添加）。粘贴链接后，点击 **Add**（添加）。支持的格式包括 `watch?v=`、`youtu.be/`、`music.youtube.com/watch?v=`、`/shorts/`、`/embed/`、`/live/`，以及仅含 11 个字符的 ID。播放列表、频道页面和其他网站会被拒绝，包含 `&list=` 的链接只会作为一个曲目获取。同一时间只会进行一次下载。点击 **Close**（关闭）可退出对话框，下载会在后台继续。音频以 MP3（160K）格式保存在数据目录的 `bardwave-library` 文件夹中，文件名为 `<title> [<id>]`。再次添加同一视频会复用已有文件。

| 消息 | 原因 | 解决方法 |
|---|---|---|
| `Not a YouTube video link` | 文本格式不被接受。 | 粘贴单个视频链接。 |
| `yt-dlp not found — downloading it once (~24 MB)…` | 首次使用时将从 GitHub 获取 `yt-dlp`。 | 等待即可。下载的文件会缓存在数据目录的 `tools` 文件夹中。 |
| `Failed to download yt-dlp: …` 或 `Downloaded yt-dlp is only N bytes — aborted` | 没有网络，或 GitHub 返回错误。 | 检查网络连接，然后重试。 |
| `Neither the Bardwave data folder nor the app-data tools folder is writable…` | 无法保存该工具。 | 使数据目录可写，或将 `yt-dlp` 放在 Bardwave 旁边或 `PATH` 中。 |
| `This platform has no prebuilt yt-dlp…` | 没有适用于此操作系统或 CPU 的发行版文件。Windows、macOS 和 Linux 提供了相应文件。 | 安装 `yt-dlp` 并将其放入 `PATH`。 |
| `ffmpeg not found on this system…` | 缺少 ffmpeg。仅 Windows 会自动下载（约 100 MB，BtbN 构建版）。 | `brew install ffmpeg`（macOS）或 `apt install ffmpeg`（Linux）。在 Windows 上，请将 `ffmpeg.exe` 放在 `Bardwave.exe` 旁边。 |
| `yt-dlp failed: …` | YouTube 已变更，或视频不可用或受地区限制。该文本是 yt-dlp 自身错误输出的结尾部分。 | 更新 `yt-dlp` 并重试。 |
| `Download finished, but no file for <id> appeared in <folder>` | yt-dlp 报告成功，但未写入任何文件。 | 检查 `bardwave-library` 文件夹和日志。 |

Bardwave 仅在缺少 `yt-dlp` 时才会下载，且从不更新它。查找顺序为：程序所在文件夹、`PATH`，然后是其自身的 `tools` 文件夹（以及早期版本中按用户划分的旧 tools 文件夹）。要更新，请用较新的版本替换该文件，或删除 `tools` 文件夹中的副本，以便重新获取新版本。进度行显示为 `Connecting…`、`Fetching video info…`、`Downloading audio… N%` 和 `Converting to MP3…`。下载的工具仅检查最小大小，不核对哈希值或签名。macOS 和 Linux 上的下载流程，以及 Windows 上的 ffmpeg 下载，均在真实机器上未验证。

```bash
pip install -U yt-dlp   # if yt-dlp came from pip
brew install ffmpeg     # macOS, when ffmpeg is missing
```

<a id="missing-files"></a>

### 缺失文件

移动过的文件会保留在声音库中，并显示为缺失：声音库中会出现一条带有数量的琥珀色横幅，每个此类声音都有警告。在某个声音上点击 **Locate…**（重新定位），或点击其所在行上的 `LOCATE…`，即可选择它的新文件。当整个文件夹已移动时，点击横幅中的 **Replace Path Prefix…**（替换路径前缀）即可重写所有路径的开头部分（会显示将更改的路径数量）。点击 **Recheck**（重新检查）会再次查找文件。叉号图标会将该声音从声音库中移除。场景、播放列表和快捷键引用的是声音 ID 而非路径，因此重新关联后仍可正常使用。

### 设置保存问题

`config.toml` 位于数据目录中，其位置取决于平台：

| 情形 | 文件夹 |
|---|---|
| 程序旁边有 `config.toml`（在 Windows 上要求该文件夹可写），或其旁边有 `portable.flag` 文件，或程序所在文件夹可写（且不在 macOS `.app` 内） | 程序所在文件夹 |
| macOS，其他情况 | `~/Library/Application Support/Bardwave` |
| Windows，其他情况（例如 `Program Files`） | `%APPDATA%\Bardwave` |
| Linux 及其他 Unix 系统，其他情况 | `$XDG_DATA_HOME/bardwave` 或 `~/.local/share/bardwave` |

该选择在启动时进行（在每个操作系统上未验证）。使用 `--profile <name>` 时，`config.toml`、声音库、`bardwave-library` 和 `tls` 文件夹会移动到该文件夹内的 `profiles/<name>/` 中。日志、`tools` 文件夹和 `recent.toml` 保留在根数据目录中。位于可写文件夹（下载文件夹、U 盘）中的程序副本会在其旁边保留自己的 `config.toml`，因此同一程序的另一个副本可能显示不同的设置。

设置看起来已重置？从顶栏的 **Profile**（配置方案）菜单中选择 **Open data folder**（打开数据目录），检查正在编辑的是哪个 `config.toml`。只保留一个。程序旁边的 `portable.flag` 文件会将数据保留在其旁边。

如果无法读取 `config.toml`，Bardwave 会显示 `config.toml could not be read`（在控制台中显示为消息，在使用 `--server-only` 时则为标准错误输出中的错误），然后停止运行。它不会使用默认值启动，因为下一次保存会覆盖声音库。修复该文件，或将其移走，即可使用默认值启动。

### macOS 上的 Gatekeeper

未签名的构建在首次打开时会被阻止。应用包会获得一个 ad-hoc 签名，以便在 Apple Silicon 上运行，但尚未对本项目进行签名和公证（未验证）。

1. 不要双击 `Bardwave.app`。
2. 右键单击（Control-单击）该应用，选择 **打开**，然后再次点击 **打开**。
3. 在 macOS Sequoia 及更高版本上，如果仍被拒绝：先尝试打开一次，然后前往“系统设置 > 隐私与安全性”，点击 **仍要打开**。
4. 或者对于通过浏览器下载的文件，清除隔离属性：

```bash
xattr -dr com.apple.quarantine /Applications/Bardwave.app
```

更多步骤见 [packaging/macos/README.md](../../packaging/macos/README.md)。

<a id="faq"></a>

## 常见问题（FAQ）

**家庭游戏需要 HTTPS 吗？**
不需要。`[server] tls` 默认为 `false`，局域网通过 HTTP 即可工作。在通过互联网发送链接之前，先开启 HTTPS。参见 [HTTPS](#https)。

**为什么链接中带有 `?k=`？**
这是访问密钥。没有它，音频流会被拒绝。

**玩家能用自己的链接打开主持人面板吗？**
不能。访问密钥永远无法打开面板的数据。玩家可以加载 `/control` 处的空页面外壳，但所有读取或更改数据的请求都需要主持人令牌，否则会返回 `401`。

**如何切断拥有链接者的访问？**
点击“ACCESS KEY”（访问密钥）行中的刷新图标，然后发送新链接。

**可以不设置访问密钥运行吗？**
可以，设置 `access_token = ""` 即可。此时控制台会显示 `ACCESS KEY OFF - ANYONE CAN LISTEN`。仅在信任的网络上使用此设置。

**Bardwave 会自动检查更新吗？**
不会。该检查默认关闭。点击 **Check for updates**（检查更新）时，才会向 GitHub 查询。勾选 **Check once a day at start**（启动时每天检查一次）复选框（`[gui] check_updates = true`），即可在启动时开启自动检查，且每天最多检查一次。不会下载或安装任何内容。该检查尚未针对已发布的版本运行（未验证）。

**主音量会被保存吗？**
启动时仅使用 `config.toml` 中的 `[audio] master_volume` 值。控制台推子不会保存其位置。

**服务器运行时可以更改端口吗？**
不可以。**Port** 和 **Bind Address** 仅在服务器停止时可以编辑，并在下次启动时生效。

**玩家需要互联网连接吗？**
在局域网中收听音频流时不需要。玩家的浏览器确实会从公共 CDN 获取听众页面的样式、图标和字体（参见[数据离开本机的情况](#what-leaves-the-machine)）。在完全没有互联网的情况下，页面的表现未验证。

**玩家能看到声音库中的哪些内容？**
音频，以及活动场景和音乐曲目的名称（截断为 120 个字符，并移除控制字符）。绝不包括文件路径或声音库 ID。

**听众数量有上限吗？**
代码对听众数量没有上限。多名听众时的行为未经测试（未验证）。

**可以不显示窗口运行吗？**
可以：使用 `--server-only`（也可以配合 `--profile <name>` 使用）。快捷键和 `[[timers]]` 在此模式下不会运行。日志会打印带有密钥的玩家链接以及本机控制页面。参见[配置参考](./configuration.md)。

<a id="security-model"></a>

## 安全模型

### 两种凭据：访问密钥与主持人令牌

|  | 访问密钥 | 主持人令牌 |
|---|---|---|
| 配置项 | `[server] access_token` | `[server] gm_token` |
| 创建 | 首次启动时，如果缺失则创建 | 首次启动时，如果缺失则创建 |
| 携带方式 | `http://<host>:<port>/?k=<key>` | `http://<host>:<port>/control?g=<token>`，或 `Authorization: Bearer <token>` |
| 可打开 | 音频流（`/ws/audio`）和状态 API（`/api/status`） | 主持人面板的数据和命令（`/api/control/*`、`/ws/control`） |
| 可执行 | 收听。状态 API 会报告听众数量和版本。 | 播放、停止和循环声音；设置音量、主音量和各总线；操作场景、短音效（stinger）、闪避（ducking）、“Stop all”（全部停止），以及音乐播放列表。读取面板状态（声音、场景和播放列表的名称、音量）。 |
| 无法执行 | 面板上的任何操作。 | 添加或移除文件、更改设置、启动或停止服务器、更改凭据、读取文件或路径。 |
| 空值 | `""` 会关闭该检查：任何人都可以收听 | `""` 会关闭远程访问。面板仅在本机响应。控制台会显示 `Remote panel is off: it opens on this computer only`，以及 **Create GM key**（创建主持人令牌）按钮。 |
| 替换 | “ACCESS KEY”行中的刷新图标 | 先点击 **New key**（新令牌），再点击 **Replace key**（更换令牌）；点击 **Cancel**（取消）则保留旧令牌。 |

页面外壳（`/`、`/control` 及其脚本和样式）是公开的，不包含任何机密。所有读取或更改面板数据的操作都需要令牌。两种凭据均由 16 个字符组成，字符取自 `a-z` 和 `0-9`，约 82 比特，由操作系统的随机源生成。比较操作以恒定时间执行。控制台会遮蔽显示主持人链接，并附带警告 **Do not share with players**（请勿与玩家分享），同时为手机提供 **Show QR**（显示二维码）按钮。面板的响应携带 `Cache-Control: no-store` 和 `Referrer-Policy: no-referrer`，因为令牌可能出现在 URL 中。服务器运行时，主持人面板由控制台窗口提供服务；使用 `--server-only` 时，则由该模式提供。

```toml
[server]
access_token = "replace-with-generated-key"  # "" turns the listener check off
gm_token = "replace-with-generated-token"    # "" disables remote panel access
```

> **警告：** 两种凭据都以明文形式保存在 `config.toml` 中。请妥善保管该文件，切勿公开包含密钥的链接。在普通 HTTP 上，凭据会以明文形式通过网络传输：请为互联网连接开启 [HTTPS](#https)。

### 本机访问

只有当请求的套接字对端为环回地址、不携带代理请求头，且其 `Host` 为 `localhost`、`127.0.0.1` 或 `[::1]` 时，该请求才会被视为本机请求。本机请求无需令牌即可访问面板，但需通过两项额外检查：

- 每个 REST 操作都必须发送 `X-Bardwave-Control` 请求头。否则服务器会返回 `403 missing control header`。其他网站的页面无法在不经过 CORS 预检的情况下添加该请求头，而服务器从不批准该预检。携带令牌的请求本身即可证明意图，无需该请求头。
- 携带 `Origin` 的本机 WebSocket 握手，其值必须与 `Host` 匹配，否则返回 `403 cross-origin request`。不带 `Origin` 的握手（非浏览器页面）可以通过。

`Host` 检查可阻止 DNS 重绑定：其他域名的页面无法利用本机的免令牌访问。同一局域网中的手机不属于本机，需要主持人令牌。

### 速率限制

只有主持人令牌受速率限制。访问密钥没有临时封锁机制，因此其强度取决于 16 个随机字符。

| 规则 | 值 | 响应 |
|---|---|---|
| 令牌错误或缺失 | 按地址计数 | 返回 `401`，并附带 `game master token required` |
| 失败尝试次数 | 60 秒内 10 次 | 返回 `429`，附带 `Retry-After` |
| 通过本机的代理 | 60 秒内 60 次失败 | `429` |
| IPv6 | 按 /64 前缀计数 | 同上 |
| 被临时封锁的地址 | 即使令牌正确，在时间窗口过去之前也会被拒绝 | `429` |

令牌错误、令牌缺失以及远程面板已禁用时，返回的结果相同。在 `/ws/control` 上，单条消息最多 4096 字节，最多允许 16 个客户端连接（第 17 个会收到 `503 too many control clients`），每个客户端每秒最多可发送 100 条命令（超出部分会被忽略，并且每秒最多返回一次 `too many commands, slow down` 错误）。对空闲客户端每 25 秒发送一次 ping，沉默约 75 秒后断开。

### 代理与隧道

带有代理请求头的请求会被视为远程请求，`Host` 不是环回域名的请求同样如此。系统会检查 16 个请求头，其中包括 `X-Forwarded-For`、`X-Forwarded-Host`、`X-Forwarded-Proto`、`X-Real-IP`、`Forwarded`、`CF-Connecting-IP`、`True-Client-IP`、`X-Client-IP` 和 `Via`。转发地址从不被采信，它们只会使请求的可信度降低。

本机上的隧道永远无法获得本机访问权限，在没有令牌的情况下，它也无法获得任何内容。普通 TCP 隧道（`ssh -R`、bore、frp），以及将 `Host` 改写为 `127.0.0.1` 的代理，看起来与主持人自己的浏览器相同，因此仅应在设置了 `gm_token` 的情况下使用。经同一代理连接的客户端共享速率限制计数器，因此一个恶意客户端可能会使其他客户端被临时封锁。轮换令牌，或在本机使用面板，是摆脱该状况的办法。这些规则已通过带有模拟请求头的单元测试覆盖。未尝试使用真实的隧道软件（未验证）。

<a id="rotating-keys"></a>

### 密钥轮换

- **访问密钥：** 点击 ACCESS KEY 行中的刷新图标。使用旧密钥的听众会立即断开连接（关闭码 `4001`），其页面会提示链接已失效。
- **主持人令牌：** 依次点击 **New key** 和 **Replace key**。使用旧令牌的远程面板会被关闭（关闭码 `4003`）。玩家的链接不受影响。在本机上未使用令牌打开的面板仍保持连接。
- 两项新凭据都会在下一次配置写入时保存到 `config.toml`。
- 链接泄露后，应同时轮换两者。密钥也可能通过日志泄露：使用 `--server-only` 时，启动行 `Listener server available at ...` 包含带有密钥的玩家链接。主持人令牌从不打印。

### 资源包导入

声音库资源包（bundle）是带有 `bundle.toml` 的 `.zip` 文件（格式为 `bardwave-bundle`，由 **Export…**（导出）写入，并由 **Import…**（导入）读取）。它可能来自任何人，因此导入器会将其视为不受信任的恶意内容：

- 只要条目包含绝对路径、`..` 或 `.` 部分、空部分、反斜杠、冒号、控制字符或符号链接，整个压缩包就会被拒绝。`bundle.toml` 中的路径也会以同样严格的方式检查。
- 文件只会写入所选的目标文件夹内，且每个父文件夹都必须解析到该文件夹内。
- 只会解压清单文件中指定的文件，并且仅限扩展名为 `wav`、`flac`、`mp3`、`ogg`、`m4a` 和 `aac` 的文件。其他文件会被跳过并发出警告，声音会在没有音频的情况下导入。
- 限制：20,000 个条目、100,000 个项目、16 MB 清单文件、每个文件 2 GB、总计 8 GB。先检查声明的大小，并统计实际读取的字节数。
- 冲突按所选的冲突处理方式解决：**Keep mine**（跳过）、**Keep both**（重命名）或 **Use the bundle's**（替换，同时还会覆盖目标文件夹中已有的音频文件）。试运行不会写入任何内容。如果导入失败或被取消，则会删除其创建的文件，声音库保持不变（通过 **Use the bundle's** 替换的文件无法恢复）。
- 快捷键不属于资源包。

<a id="what-leaves-the-machine"></a>

### 数据离开本机的情况

默认情况下，除服务器提供给受邀玩家的内容和路由器请求外，不会向任何人发送任何数据。更新检查在开启前保持关闭。代码中的对外 Web 请求仅来自更新检查和工具下载。代码中没有任何遥测代码。

| 请求 | 时机 | 目的地 |
|---|---|---|
| 听众音频流 | 玩家连接时 | 能够访问该端口的设备：局域网，或在端口已映射或转发时的互联网。Opus 音频，以及场景和曲目名称。 |
| UPnP 发现与映射 | `upnp = true`（默认） | 局域网内的组播、对常用网关地址的探测，然后是路由器 |
| 更新检查 | **Check for updates**，或 `check_updates = true` | `api.github.com` 的最新版本文档，User-Agent 为 `Bardwave/<version>`，8 秒时限，256 KiB 上限。发布页面仅在点击后才会在浏览器中打开。 |
| yt-dlp 下载 | 首次使用 YouTube 且缺少该工具时 | yt-dlp 的 GitHub 发行版文件 |
| ffmpeg 下载 | 首次在 Windows 上使用 YouTube 且缺少该工具时 | BtbN FFmpeg 构建版的 GitHub 发行版文件 |
| YouTube 音频 | 添加 YouTube 链接时 | YouTube，经由 yt-dlp |

还有两点需要说明。地址检测不发送任何数据包。此外，听众页面会让玩家的浏览器（而非 Bardwave）请求 `cdn.tailwindcss.com`、`code.iconify.design` 和 `fonts.googleapis.com`。这些主机能够看到玩家的 IP 地址。浏览器是否同时发送页面地址取决于浏览器设置（未验证）。主持人页面只加载其自身的文件。

<a id="https"></a>

### HTTPS

`[server] tls = true` 会在同一端口上提供 HTTPS 和 `wss://`，链接以 `https://` 开头。在未设置 `tls_cert` 和 `tls_key` 的情况下，Bardwave 会在数据目录的 `tls` 文件夹中生成一个自签名证书，有效期为 397 天。在下次启动时，若剩余有效期不足 30 天，或本机当前的任何局域网地址均未被覆盖，证书会被替换。SHA-256 指纹（fingerprint）会显示在控制台中并写入日志，因此可以与浏览器警告进行比对。**Regenerate certificate**（重新生成证书）会生成新证书。该更改在下次启动时生效。`tls_cert` 和 `tls_key` 必须同时设置。无法读取的证书会阻止服务器启动，而不会回退到 HTTP。

```toml
[server]
tls = true
tls_cert = "/etc/ssl/bardwave/fullchain.pem"   # set both or neither
tls_key = "/etc/ssl/bardwave/privkey.pem"
```

> **注意：** 每部手机都必须信任自签名证书。该情况尚未在真实的 iPhone 或 Android 手机上测试。完整步骤见 [HTTPS（安全连接）](./https.md)。

<a id="not-verified-on-real-hardware"></a>

## 真实硬件上未验证

以下功能已实现，并由测试或 CI 构建覆盖，但尚未在真实机器上运行：

- Windows 安装程序和 Linux 软件包（AppImage、`.deb`）。
- macOS `.app` 和 `.dmg`、本地网络提示、Gatekeeper、签名与公证。
- macOS、Windows 和 Linux/X11 上的全局快捷键。
- MIDI：仅有单元测试，默认关闭，未连接到控制台。
- Windows（WASAPI）和 Linux（ALSA/JACK）上的音频设备名称、设备枚举以及第二个（试听）音频流。切换设备仅在 macOS 上运行过。
- 各操作系统上数据目录的选择。
- HTTPS：仅有自动化测试。未在真实手机上信任，也未使用真实的 Let's Encrypt 证书链测试过。
- 针对真实路由器的 UPnP（包括 `718` 和 `725` 响应以及双重 NAT 的情形），以及 Windows 防火墙规则的创建。
- 真实 VPN 客户端和 TUN 模式代理下的 VPN 提示。
- 使用真实隧道软件时的代理与隧道规则。仅测试了模拟请求头。
- iOS Safari 和 Android Chrome 上的听众页面（包括在无互联网访问时的显示效果）。
- 针对已发布版本的更新检查。
- macOS 和 Linux 上的 yt-dlp、ffmpeg 下载，以及 Windows 上的 ffmpeg 下载。
- 同时存在大量听众时的行为。
- 缓冲区和断续的建议源自代码，而非实际游戏场次。
- Windows 和 Linux 上的配置方案、首次运行向导以及界面缩放。
