# 快速入门

Bardwave 是面向桌上角色扮演游戏的桌面氛围音控制台。你加载自己的声音、搭建场景，然后点击按钮即可播放。混音通过网络以音频流发送到每位玩家的浏览器，因此玩家只需打开一个链接。本指南涵盖从安装到首个游戏场次的全过程。完整的配置参考见 [../../README.md](../../README.md)。

> **注意：** Bardwave 仍处于开发中。本文描述的部分功能——配置方案（profile）、HTTPS、首次运行向导、界面缩放——来自 `swarm/integration` 分支，可能比最新的发行版更新。尚未在真实硬件上测试的内容标有 **（未验证）**。

## 目录

- [Bardwave 简介](#what-bardwave-is)
- [系统要求](#requirements)
- [安装](#install)
- [首次启动](#first-launch)
- [首批声音的添加](#add-your-first-sounds)
- [场景的创建](#build-a-scene)
- [服务器启动](#start-the-server)
- [链接分发](#give-players-the-link)
- [无头模式](#headless-mode)
- [控制台导览](#a-tour-of-the-console)
- [界面缩放](#interface-scale)
- [数据存放位置](#where-the-data-lives)
- [常见故障](#common-problems)
- [真实硬件上未验证](#not-verified-on-real-hardware)
- [后续步骤](#next-steps)

<a id="what-bardwave-is"></a>

## Bardwave 简介

Bardwave 运行在主持人（GM）的电脑上。它从你自己的文件中播放音乐、氛围音和音效，在控制台中实时混音，并将结果以一路 Opus 音频流（20 ms 帧，约 64 kbit/s）推送到每位玩家的浏览器。玩家无需安装任何软件，只需打开链接并点击按钮。你的文件保留在你自己的电脑上，玩家只会听到混音，因此你可以在游戏场次进行中更改某个声音或其音量，且无需任何人下载任何内容。

没有账户，也没有云服务。默认安装本身不会发出任何网络请求：检查更新默认关闭，YouTube 声音的下载仅在你使用该功能时才会进行。

主要组成部分如下：

- 带有分组和标签的**声音库**；
- **场景**：保存的声音组合，包含音量与淡变，另外还有**短音效**（stinger，单次播放）和**发射器**（emitter，按随机间隔播放声音的声源）；
- 带有播放列表的**音乐栏**；
- 三条**总线**（bus：music、ambience、sfx），并支持闪避（ducking）；
- 带有玩家链接的**网络服务器**，以及供主持人使用的手机控制页面。

<a id="requirements"></a>

## 系统要求

| 平台 | 运行发行版 | 从源码构建 |
|---|---|---|
| Windows 10 或 11 | 一块声卡。 | Rust 1.85 或更高版本，可从 [rustup.rs](https://rustup.rs) 获取。 |
| macOS 11 或更高版本 | 一块声卡。 | Rust 1.85 或更高版本。无需系统库。 |
| Linux | 一块声卡、GTK 3、ALSA 和 libxkbcommon。 | Rust 1.85 或更高版本，另需下文列出的构建软件包。 |

> **注意：** Rust 版本取自 README，`Cargo.toml` 中并未固定该版本。

如果启动时无法打开任何输出设备，Bardwave 会记录一条警告，并继续向玩家推送音频流。底部的状态栏会说明桌面端为何没有声音。

Linux 运行时依赖库（Debian 和 Ubuntu）：

```bash
sudo apt install libgtk-3-0 libasound2 libxkbcommon0
```

Linux 构建所需的软件包（Debian 和 Ubuntu）。CI 工作流除安装这些软件包外，还会安装本指南未列出的三个 `libxcb-*-dev` 软件包：

```bash
sudo apt install pkg-config libgtk-3-dev libasound2-dev libxkbcommon-dev libssl-dev
```

使用 YouTube 链接时，在 Linux 和 macOS 上还需要 `ffmpeg`。参见[首批声音的添加](#add-your-first-sounds)。

<a id="install"></a>

## 安装

### 从发行版安装

发行版发布在 [GitHub 发布页面](https://github.com/adamantida/Bardwave/releases)。选择适合你系统的文件：

| 系统 | 文件 | 说明 |
|---|---|---|
| Windows | `Bardwave-v<version>-windows-x86_64.zip` | 便携版压缩包。解压后生成一个文件夹，其中包含 `Bardwave.exe`、带注释的 `config.toml`、`README.md`、`README.txt` 和 `LICENSE`。 |
| Windows | `Bardwave-v<version>-windows-x86_64-setup.exe` | 安装程序。安装到 Program Files，并可为 TCP 端口 8080 添加防火墙规则。 |
| Windows | `Bardwave-v<version>-windows-x86_64.exe` | 仅含程序本体，旁边没有 `config.toml`。 |
| macOS | `Bardwave-v<version>-macos-universal.dmg` | 除非发行版已签名，否则未签名。 |
| Linux | `Bardwave-v<version>-linux-x86_64.tar.gz` | 解压后运行 `bardwave`。 |
| Linux | `Bardwave-v<version>-linux-x86_64.AppImage` | **（未验证）** |
| Linux | `bardwave_<version>-1_amd64.deb` | **（未验证）** |
| 全部 | `SHA256SUMS` | 发行版文件的校验和。 |

**Windows，便携版压缩包：**

1. 将压缩包解压到一个可写入的文件夹，例如 `Documents\Bardwave`。不要使用 `Program Files`。
2. 运行 `Bardwave.exe`。旁边的 `config.toml` 会将你的数据保存在该文件夹中。

**Windows，安装程序：**

1. 运行 `-setup.exe` 文件。它会请求管理员权限。
2. 若玩家需要连接，保持勾选 **Allow Bardwave through Windows Firewall (inbound TCP 8080, private and public networks) so players can connect**（允许 Bardwave 通过防火墙）。**（未验证）**
3. 安装程序会添加开始菜单项和可选的桌面快捷方式。它不会安装 `config.toml`，因此你的数据会保存到 `%APPDATA%\Bardwave`。卸载不会删除你的数据。

**macOS：**

1. 打开 `.dmg` 文件，将 **Bardwave** 拖到 **应用程序**。
2. 首次运行时不要双击应用。右键单击（或按住 Control 单击）**Bardwave**，然后选择 **打开**，再次选择 **打开**。
3. 若 macOS 仍然拒绝运行，先打开一次该应用，然后前往 **系统设置 > 隐私与安全性**，选择 **仍要打开**。**（未验证）**
4. 首次启动时，macOS 可能会询问是否允许 Bardwave 查找本地网络中的设备。选择允许，否则局域网（LAN）中的玩家无法连接。**（未验证）**

**Linux，tar.gz 压缩包：**

```bash
tar -xzf Bardwave-v<version>-linux-x86_64.tar.gz
cd Bardwave-v<version>-linux-x86_64
./bardwave
```

**Linux，`.deb`：** `sudo apt install ./bardwave_<version>-1_amd64.deb`，然后从菜单启动 **Bardwave**，或运行 `bardwave`。**（未验证）**

### 从源码构建

克隆仓库，然后以 release 模式运行：

```bash
git clone https://github.com/adamantida/Bardwave.git
cd Bardwave
cargo run --release
```

在 Windows 上也可以改用 `build.bat`。不带参数时生成 debug 构建；`release` 生成 release 构建，`test` 运行全部测试：

```bat
build.bat release
build.bat test
```

可执行文件在 Windows 上为 `target\release\soundshared.exe`，在其他系统上为 `target/release/soundshared`。可将其重命名为 `Bardwave.exe` 或 `bardwave`。

> **注意：** 从 `target/release` 运行的可执行文件会将数据保存在其旁边，即 `target/` 目录内。运行 `cargo clean` 会删除这些数据。参见[数据存放位置](#where-the-data-lives)。

<a id="first-launch"></a>

## 首次启动

控制台窗口以 1280 × 800 点的尺寸打开，采用深色广播布局：顶栏、左侧的声音库、右侧的网络面板、音乐栏，以及底部的状态栏。窗口不能缩小到 960 × 640 点以下。

在全新安装且声音库为空时，首次运行向导会以卡片形式显示在窗口中央。它不会阻塞控制台的使用，对话框打开时会自动避让。向导共有三页：

| 页面标题 | 页面上的按钮 |
|---|---|
| Add your sounds | **Add sound**（添加声音）、**Add folder**（添加文件夹）、**YouTube**。 |
| Build a scene | **Save current mix as scene**（将当前混音保存为场景）。声音库中有声音后，该按钮才可用。 |
| Share with players | **Start server**（启动服务器），失败后为 **Retry server**（重试服务器）。服务器运行时：玩家链接及其 **Copy**（复制）按钮，以及 **Show QR code**（显示二维码）。 |

当应用状态满足某页要求时，该页会自动打勾：声音库中有声音、已存在场景、服务器正在运行。

- **Next**（下一步）和 **Back**（上一步）用于在页面之间切换。最后一页以 **Done**（完成）代替 **Next**。
- **Done** 关闭卡片，此后启动时向导不再打开。
- **Skip**（跳过）关闭卡片。若希望向导在下次启动时不再出现，先勾选 **Don't show again**（不再显示）。随时可通过 **Help > Getting started**（帮助 > 入门）重新打开。

> **注意：** 只有在声音库为空且向导未被永久关闭时，向导才会打开。如果配置中已有声音库，向导会立即标记为已完成，不会打开。

<a id="add-your-first-sounds"></a>

## 首批声音的添加

文件夹导入可读取以下音频格式：`wav`、`flac`、`mp3`、`ogg`、`m4a` 和 `aac`。超过 60 秒的文件（`config.toml` 中的 `stream_threshold_secs`）会被实时解码，而不会整段保存在内存中。

> **注意：** **Add Sound** 打开的文件选择器中有一个“Audio Files”过滤器，列出 `mp3`、`wav`、`ogg`、`flac` 和 `opus`。以此方式选择 `m4a` 或 `aac` 文件，以及播放 `opus` 文件，均为 **（未验证）**。文件夹导入支持 `m4a` 和 `aac`。

1. 在 **Sound Library**（声音库）面板中点击 **Add Sound**，选择一个或多个文件。也可以在向导中点击 **Add folder**，或在声音库工具栏中点击 **Folder…**（文件夹），以扫描整个文件夹及其子文件夹。Bardwave 只保存每个文件的路径，不会复制文件。
2. 为声音设置分组或标签。列表左侧的 **Groups**（分组）栏显示 **All Sounds**（全部声音）、**Ungrouped**（未分组），以及 **Categories**（分类）下你自己的分组。栏中的 **+** 按钮用于创建分组。点击声音行上的分组块可移动该声音，其提示为 **Change group**（更改分组）；点击标签文字可编辑其标签，其提示为 **Edit tags**（编辑标签）。使用搜索框 **Search name or tag…**（按名称或标签搜索）进行筛选。
3. 右键单击一个较短的声音，选择 **Use As Stinger**（用作短音效）。短音效是一种单次播放的冲击音效（例如雷鸣），它会叠加在场景之上播放，并在 **Scenes**（场景）面板中获得一个触发键。**Remove Stinger Flag**（移除短音效标记）可撤销此设置。
4. 用该行上的循环开关将循环声音（例如雨声）设为重复播放，其提示为 **Repeat: on**（重复：开）或 **Repeat: off**（重复：关）。

每一行还有耳机按钮（仅在本机播放该声音）、音量滑块，以及用于从声音库中移除该声音的 **✕**。

如果文件被移动或删除，该行会显示 **File not found**（未找到文件）。使用 **Locate…**（重新定位）重新指定该文件，或使用 **Recheck**（重新检查）再次扫描。

### 来自 YouTube 的声音

1. 点击 **Add Sound** 旁边的 **YouTube**。
2. 将视频或音乐链接粘贴到 **Video or music link**（视频或音乐链接）中，然后点击 **Add**（添加）。
3. 等待下载完成。只下载音频，且只下载一次。音频会转换为 MP3，保存到数据目录下的 `bardwave-library/`，此后与任何本地声音一样播放。关闭对话框不会停止正在进行的下载。

> **警告：** 首次下载需要 `yt-dlp` 和 `ffmpeg`。在 Windows 上，Bardwave 会将两者下载到 `tools/` 中（分别约 24 MB 和约 230 MB）。在 Linux 和 macOS 上，`yt-dlp` 会自动获取，但你必须自行安装 `ffmpeg`：
>
> ```bash
> sudo apt install ffmpeg    # Debian or Ubuntu
> brew install ffmpeg        # macOS, Homebrew
> ```
>
> 缺少时，对话框会显示 `ffmpeg not found on this system`。程序旁边或 `PATH` 中的 `yt-dlp` 或 `ffmpeg` 副本，会优先于任何已下载的副本使用。

<a id="build-a-scene"></a>

## 场景的创建

场景是一组保存的声音及其音量。场景显示在 **Scenes** 面板中，该面板位于声音库面板内、声音列表的上方。

1. 手动播放几个声音，并用滑块调整它们的音量。
2. 点击 **Save Mix As Scene**（将混音保存为场景）。只有在有声音播放时，该按钮才可用。在窄布局中，其文字会缩短为 **Save Mix**（保存混音），或缩为图标。向导中对应的同一操作称为 **Save current mix as scene**。
3. 在 **Scene name**（场景名称）输入框中为场景命名。之后在面板中点击该场景的按钮即可切换到它。切换时使用淡变过渡。
4. 使用 **Stop Scene**（停止场景）淡出当前活动场景。**Fade Out**（淡出）会在过渡时间内淡出所有正在播放的内容。

场景还可以包含短音效、循环触发键和发射器。发射器会按随机间隔播放随机声音。你可以在场景的 **Emitters & music**（发射器与音乐）编辑器中设置发射器和场景的音乐播放列表。

<a id="start-the-server"></a>

## 服务器启动

服务器将混音发送给玩家。在游戏场次开始前启动它。

1. 打开右侧的 **Network Server**（网络服务器）面板（标题为 **02 / Network**）。
2. 点击 **Start Server**。运行时，标题变为 **Live**（播出中）。
3. **Listen URL**（监听地址）显示地址。点击 **Copy Link**（复制链接）复制玩家链接。

默认端口为 `8080`。面板会显示以下地址：

| 地址 | 使用者 |
|---|---|
| `http://192.168.x.x:8080` | 同一网络中的玩家。 |
| `http://127.0.0.1:8080` | 你自己，用于在本机测试听众页面。面板会在 **Local**（本机）行显示该地址。 |
| `http://<global-ip>:8080` | 互联网上的玩家。当路由器转发端口时，会在 **Internet**（互联网）行显示。 |

要停止服务器，点击 **Stop Server**（停止服务器）。

如果服务器无法启动，面板会显示 **Error**（错误）及相应消息，顶栏会显示 **Server failed — see panel**。检查消息内容，然后点击 **Retry Server**。

### 自动端口转发（UPnP）

服务器启动时，Bardwave 会通过 UPnP 请求路由器转发该端口。服务器停止时会移除该映射。要关闭此功能，在 `config.toml` 中设置 `upnp = false`，此时 **Internet** 行会隐藏。

**Internet** 行会显示结果。常见消息：

- **Internet: no UPnP router**：在路由器上启用 UPnP，或手动转发端口。
- **Internet: blocked by a second NAT**：路由器的外部地址为私有地址。在上游设备上转发端口，或向运营商申请公网 IP。
- **VPN active**：本机上的 VPN 会接管全部流量。互联网上的玩家需要连接时，请关闭 VPN。

在 Windows 上，面板还可能显示 **Firewall: port 8080 not opened**。也要允许 Bardwave 通过公用网络，或以管理员身份运行一次。

<a id="give-players-the-link"></a>

## 链接分发

玩家链接的格式如下：

```text
http://<address>:8080/?k=<access key>
```

如果开启 HTTPS，链接以 `https://` 开头。参见 [HTTPS（安全连接）](./https.md)。

1. 使用 **Copy Link** 将链接发送给每位玩家。
2. 玩家在任意浏览器中打开链接，然后点击 **Establish Link**（建立连接）。

链接以 `?k=<access key>` 结尾。没有该访问密钥，即使端口对互联网开放，也没有人能收听。

> **警告：** 只有在需要吊销链接时，才点击 **ACCESS KEY**（访问密钥）旁边的刷新按钮。新密钥会立即使旧链接失效，并使所有使用旧链接的人断线。之后请重新发送新链接。

### 主持人面板

**GM panel link**（主持人面板链接）会打开一个适合手机的控制页面，地址为 `/control`。它有自己的令牌，玩家链接永远无法打开它。面板会以部分隐藏的形式显示该链接。点击它可复制完整链接，或使用 **Show QR**（显示二维码）以便用摄像头扫描。**New key**（新令牌）用于更换令牌（更换前会先询问）。

> **警告：** 远程控制默认开启。任何持有 `?g=` 链接的人都可以点击主持人的按钮。请勿泄露此链接。若只允许本机访问控制页面，请在 `config.toml` 中设置 `gm_token = ""`。此后面板会显示 **Remote panel is off: it opens on this computer only**，并带有 **Create GM key**（创建主持人令牌）按钮，用于重新开启远程访问。

<a id="headless-mode"></a>

## 无头模式

无头模式（headless）让服务器在没有控制台窗口的情况下运行。适用于放在路由器旁边的备用电脑，或通过手机控制的游戏场次。

使用发行版时，在终端中运行：

```bash
Bardwave.exe --server-only    # Windows
./bardwave --server-only      # Linux
```

从源码运行：

```bash
cargo run --release -- --server-only
```

无头模式会：

- 加载 `config.toml` 中保存的声音库；
- 立即启动听众服务器；
- 在日志中记录玩家链接和控制链接。

日志中会显示以下行：

- `Listener server available at …` 给出玩家链接。
- `Control page (this PC only): …/control` 给出本机的控制页面。
- `Remote control: …/control?g=<gm_token from config.toml>` 给出远程面板链接。仅在远程控制开启时才会记录该链接。

在同一台电脑上，`http://127.0.0.1:8080/control` 无需密钥即可打开。在其他设备上，使用 `http://<address>:8080/control?g=<gm_token>`。令牌位于 `config.toml` 的 `[server]` 部分中。

按 **Ctrl+C** 停止。在 Windows 上，如果从资源管理器或快捷方式启动，它会为日志单独打开一个控制台窗口，关闭该窗口即可停止运行。

`--profile` 和 HTTPS 设置在无头模式下同样有效。

> **注意：** 无头模式不运行快捷键和场次定时器。要修改声音库，需在控制台中编辑，然后重新启动服务器。

### 配置方案

每个战役都可以拥有自己的配置和声音库。通过 `--profile` 启动指定的配置方案，例如 `cargo run --release -- --profile "Curse of Strahd"`（`--profile=<name>` 同样有效，`--profile default` 即默认配置方案）。名称可包含字母、数字、空格、`-` 和 `_`，最多 32 个字符。顶栏中的 **Profile**（配置方案）菜单可在各配置方案之间切换。切换时会保存你的工作，并以另一个配置方案重新启动 Bardwave。

<a id="a-tour-of-the-console"></a>

## 控制台导览

### 顶栏

| 控件 | 功能 |
|---|---|
| 文字标志与 **Broadcast Console**（广播控制台） | 品牌名与副标题。窗口较窄时不显示副标题。 |
| **Master**（主音量）滑块及百分比 | 主音量。提示：**Master volume**。 |
| **STOP ALL**（全部停止） | 立即停止所有内容：声音、当前活动场景、音乐以及耳机试听。提示：**Stop all sounds immediately (panic)**。没有内容播放时按钮呈灰显，但仍然有效。 |
| **REC**（录制） | 将玩家听到的混音录制为 WAV 文件。录制时显示 **Stop rec**（停止录制）。旁边的文件夹按钮用于选择录音文件夹。 |
| **Profile** | 当前配置方案的名称。其菜单列出最近使用的配置方案，并包括 **New profile…**（新建配置方案）、**Manage profiles…**（管理配置方案）和 **Open data folder**（打开数据目录）。 |
| **Help** | 包含 **Getting started**，用于重新打开向导。 |
| 状态 | **Live**、**Starting**（启动中）、**Error**、**Off Air**（未播出），第二行显示 **Server running**（服务器运行中）或 **Server failed — see panel** 等内容。 |

按钮上的文字以大写字母绘制，但本指南中的文字与代码中的写法一致。窗口较窄时，顶栏会缩短文字，并省略部分内容。

### 声音库

左侧面板 **01 / Library**（标题为 **Sound Library**）列出声音和分组。**Add Sound** 用于添加文件，**YouTube** 用于添加链接，**Folder…** 用于扫描文件夹，**Export…**（导出）和 **Import…**（导入）用于处理 `.zip` 资源包（bundle）。列表下方固定有一个 **Master Output**（主输出）栏，其中包含音量滑块和 **Mute**（静音）/ **Unmute**（取消静音）按钮。

### 场景面板

**Scenes** 面板位于声音列表上方。其中包含场景按钮、**Stingers**（短音效）触发键、**Stop Scene**、**Fade Out**，以及 **Transition**（过渡）时间；**Auto**（自动）表示使用各场景自身的淡变。使用 **New**（新建）创建空场景，使用 **Save Mix As Scene** 保存当前混音，使用 **Show**（显示）或 **Hide**（隐藏）折叠该面板。

### 音乐栏

音乐栏位于状态栏上方。

- 用于控制当前播放列表的按钮：**Previous track**（上一曲目）、**Play**（播放） / **Pause**（暂停） / **Resume**（继续）、**Stop**（停止）和 **Next track**（下一曲目）。
- 当前播放行显示曲目及其位置，并带有进度条。空闲时显示 **No music playing**（无音乐播放）。
- **Shuffle**（随机播放）和 **Repeat**（重复）用于设置播放模式，后者可选 off、all 或 one。
- **Music…**（音乐）打开 **Music & scenes**（音乐与场景）对话框，其中有 **Playlists**（播放列表）和 **Scenes** 两个选项卡。播放列表作为背景音乐播放。场景可以启动某个播放列表，**Scenes** 选项卡可进入各场景的 **Emitters & music** 编辑器。

### 网络面板

右侧面板 **02 / Network**（标题为 **Network Server**）自上而下包含：

- 服务器状态，以及 **Start Server** / **Stop Server** 和 **Copy Link**；
- **Listen URL**、**Local** 和 **Internet** 行，以及各项网络警告；
- 带有刷新按钮的 **ACCESS KEY** 行，以及 **GM panel link**；
- **Listeners**（听众）计数及其峰值；
- **Mix**（混音）部分：总线推子、**Duck**（闪避）、**Hotkeys**（快捷键）、**Timers**（定时器）、闪避设置、响度归一化（normalization），以及 **Interface**（界面缩放滑块）；
- **Audio output**（音频输出），用于声卡和独立的耳机设备；
- **Updates**（更新），其中包含 **Check for updates**（检查更新）；
- **Secure connection (HTTPS)**（安全连接）；
- **Configuration**（配置）：**Port**（端口）和 **Bind Address**（绑定地址），仅在服务器停止时可编辑。

最底部的状态栏显示版本、本机输出、格式与声音库的数量，并在更新检查发现新版本时显示 **Update available**（有可用更新）链接。

<a id="interface-scale"></a>

## 界面缩放

使用此功能可放大或缩小控制台。

1. 打开 **Network** 面板，滚动到 **Mix** 部分的末尾。
2. 找到 **Interface**，然后找到 **Scale**（缩放）滑块。
3. 拖动以设置缩放比例。范围为 80% 到 160%，步长为 5%。
4. 点击 **Reset**（重置）恢复到 100%。缩放比例为 100% 时，该按钮灰显。

缩放在松开滑块后才应用，并且该设置会保存在 `config.toml` 中。键盘缩放（Ctrl 加 + 或 -）已关闭，因此滑块是唯一的控制方式。

配置项为 `[gui] ui_scale`。仅当其值与 1.0 不同时才会写入：

```toml
[gui]
ui_scale = 1.3   # 1.0 = 100 %; allowed range 0.8 to 1.6
```

手动编辑后超出范围的值会被限制在该范围内。无效值会回退到 1.0。

窗口的最小尺寸仍为 960 × 640 点；如果更大的缩放比例会使窗口过小，窗口会随之增大。

<a id="where-the-data-lives"></a>

## 数据存放位置

Bardwave 将 `config.toml`、声音库、录音和下载的工具保存在**数据目录**中。该位置仅在启动时选择一次，采用下表中第一条符合的规则：

| 情形 | 数据目录 |
|---|---|
| `config.toml` 已位于程序旁边（在 Windows 上仅当该文件夹可写时），或该处存在名为 `portable.flag` 的文件 | 程序所在文件夹。 |
| 程序所在文件夹可写，且不在 macOS `.app` 内 | 程序所在文件夹。 |
| macOS `.app` | `~/Library/Application Support/Bardwave` |
| Windows，例如安装到 Program Files | `%APPDATA%\Bardwave` |
| Linux | `$XDG_DATA_HOME/bardwave`，或 `~/.local/share/bardwave` |

如果无法创建所选的系统文件夹，或缺少主目录变量，Bardwave 将改用程序所在文件夹。

| 项目 | 位置 |
|---|---|
| `config.toml`、下载的声音 `bardwave-library/`、HTTPS 文件 `tls/` | 当前配置方案的数据目录。 |
| 录音 `recordings/` | 当前配置方案的数据目录，除非设置了 `recordings_dir`。 |
| 日志 `bardwave.log`（仅在 `file_logging = true` 时），下载的工具 `tools/`，最近使用的配置方案 `recent.toml` | 共享的根数据目录。 |

对于命名的配置方案，其配置和声音库位于数据目录内的 `profiles/<name>/` 中。默认配置方案直接使用数据目录本身。

### 便携模式与重置

要将所有数据保存在程序旁边，在程序所在文件夹中创建一个名为 `portable.flag` 的空文件。Windows 压缩包自带的 `config.toml` 起到同样的作用。在 macOS `.app` 内，程序所在文件夹为 `Contents/MacOS` **（未验证）**；修改应用程序包可能导致其签名失效。

要重置 Bardwave，删除数据目录即可。其他配置方案也存放在该目录中，会一并删除。卸载不会删除该目录。

<a id="common-problems"></a>

## 常见故障

| 现象 | 可能原因 | 处理方法 |
|---|---|---|
| 玩家无法通过局域网连接 | 防火墙阻止该端口。 | 允许 Bardwave 在专用网络和公用网络上通过防火墙。在 Linux 上，开放 TCP 端口 8080。在 macOS 上，在 **系统设置 > 隐私与安全性** 中为 Bardwave 允许 **本地网络**。**（未验证）** |
| 没有 **Internet** 行 | `config.toml` 中的 `upnp = false`。 | 设置 `upnp = true`，或手动转发端口。 |
| **Internet: no UPnP router** | 路由器上的 UPnP 已关闭，或没有路由器作出响应。 | 启用 UPnP，或手动转发端口。 |
| **Internet: blocked by a second NAT** | 路由器的外部地址为私有地址。 | 在上游设备上转发端口，或获取公网 IP。 |
| **VPN active** | VPN 接管了本机的流量。 | 为互联网上的玩家关闭 VPN。 |
| **Firewall: port 8080 not opened**（Windows） | Bardwave 无法添加其防火墙规则。 | 也要允许 Bardwave 通过公用网络，或以管理员身份运行一次。 |
| 控制台无法启动，并显示 `config.toml could not be read` | `config.toml` 中的 TOML 无效。 | 修复该文件，或将其移走，以默认值启动。 |
| 其他窗口获得焦点时，快捷键不会触发 | 全局快捷键已关闭。 | 在 **Hotkeys** 对话框中勾选全局快捷键复选框，或在 `[gui]` 中设置 `global_hotkeys = true`。在 macOS 和 Linux 上 **（未验证）**；在 Linux 上它们仅在 X11 下有效。 |
| YouTube 显示 `ffmpeg not found on this system` | Linux 或 macOS 上缺少 `ffmpeg`。 | 使用包管理器安装 `ffmpeg`。 |
| 某个声音显示 **File not found** | 文件已移动。 | 点击 **Locate…**，或点击 **Recheck**。 |
| 面板显示 **Error** | 服务器无法启动，例如端口已被占用。 | 查看面板中的消息，在服务器停止时于 **Configuration** 下更改端口，然后点击 **Retry Server**。 |

<a id="not-verified-on-real-hardware"></a>

## 真实硬件上未验证

以下各项已实现，并有测试或 CI 构建覆盖。它们尚未在真实机器上运行：

- Windows 安装程序和 Linux 软件包（AppImage、`.deb`）。
- macOS 的 `.app`、`.dmg`、通用二进制文件、本地网络提示，以及签名。
- macOS、Windows 和 Linux（X11）上的全局快捷键。
- MIDI。默认关闭，未与控制台连接，并且需要 `--features midi`。
- Windows（WASAPI）和 Linux（ALSA 或 JACK）上的音频设备名称。
- 使用真实路由器时的 UPnP。
- iOS Safari 和 Android Chrome 上的听众页面，包括添加到主屏幕的安装。
- 真实手机上使用自签名证书的 HTTPS。
- 各操作系统上的数据目录选择，以及 Windows 和 Linux 上的配置方案、首次运行向导和界面缩放。

<a id="next-steps"></a>

## 后续步骤

- 与你所在网络之外的玩家游戏之前，先阅读 [HTTPS（安全连接）](./https.md)。
- 使用 `--profile` 为另一个战役设置第二个配置方案。
- 阅读[场景、音乐与快捷键](./scenes-music-hotkeys.md)、[声音库](./soundboard-library.md)和[网络、玩家与主持人远程面板](./network-remote.md)，深入了解各部分内容。
- 有关音频输出、REC 和配置方案，阅读[音频设备、录制、配置方案与更新](./audio-session-tools.md)。
- 阅读[配置参考](./configuration.md)，完整的配置参考见 [../../README.md](../../README.md)。
- 遇到问题时，阅读[故障排查、常见问题（FAQ）与安全](./troubleshooting-security.md)。
