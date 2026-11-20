# 音频设备、录制、配置方案与更新

本页介绍 Bardwave 控制台的四个部分：选择声卡、将游戏场次录制为 WAV、运行彼此独立的战役配置方案（profile），以及可选的检查更新。内容依据 `src/audio`、`src/gui`、`src/profiles.rs`、`src/paths.rs` 和 `src/updates.rs` 中的代码编写。按钮及各类文字按源代码中的大小写引用。控制台本身以大写字母绘制按钮和区块标题（例如 `REC`、`REFRESH DEVICES`、`AUDIO OUTPUT`）。

设备处理基于 cpal 编写，仅在 macOS（CoreAudio）上进行过测试。Windows（WASAPI）和 Linux（ALSA/JACK）上的行为未验证。

## 目录

- [音频输出设备](#audio-output-devices)
  - [选择输出与试听设备](#choosing-the-output-and-preview-devices)
  - [回退与错误信息](#fallback-and-error-messages)
  - [缓冲区大小、采样率与声道](#buffer-size-sample-rate-and-channels)
- [游戏场次录制](#session-recording)
  - [录制内容](#what-is-recorded)
  - [文件保存位置与命名方式](#where-files-go-and-how-they-are-named)
  - [大小限制与分段](#size-limits-and-splitting)
  - [录制设置](#recording-settings)
  - [录制故障排查](#recording-troubleshooting)
- [配置方案](#profiles)
  - [以配置方案启动](#starting-with-a-profile)
  - [切换配置方案](#switching-profiles)
  - [管理配置方案](#managing-profiles)
  - [最近使用的配置方案](#recent-profiles)
- [检查更新](#update-check)

<a id="audio-output-devices"></a>

## 音频输出设备

**AUDIO OUTPUT**（音频输出）区块位于服务器面板中，介于混音设置与 **Updates**（更新）区块之间。它包含两个下拉列表，每个下拉列表下方各有一行状态行：

| 下拉列表 | 文字 | 用途 |
|---|---|---|
| 主输出 | `Output` | 播放混音的设备。其“无”选项为 `System default (<name>)`；若未知默认设备，则仅为 `System default`。 |
| 试听输出 | `Preview` | 播放声音试听（preview）的设备（通常为耳机）。其“无”选项为 `Same as output`。 |

修改会立即生效。音频引擎会停止其音频流，并按新的选择重新打开，无需重新启动应用，曲目继续播放。该选择也会立即保存到 `config.toml`。

<a id="choosing-the-output-and-preview-devices"></a>

### 选择输出与试听设备

1. 打开 **AUDIO OUTPUT** 区块。
2. 打开 `Output` 下拉列表并选择一个设备。若要恢复默认，选择 `System default (<name>)`。
3. 打开 `Preview` 下拉列表并选择耳机。若要在主输出上播放试听，选择 `Same as output`。
4. 阅读每个下拉列表下方的状态行。
5. 插入或拔出设备后，点击 `Refresh devices`。

将鼠标悬停在列表中的设备上可查看其摘要，例如 `2 ch · 44.1 / 48 / 96 kHz · system default`。摘要由设备支持的最大声道数，以及该列表中受支持的采样率生成：22.05、44.1、48、88.2、96 和 192 kHz。较长的设备名称在下拉列表中会用省略号截短。

该选择保存在 `[audio]` 中：

```toml
[audio]
output_device = "USB Headphones"   # main output; omit for the system default
preview_device = "Built-in Output" # previews; omit to play them on the main output
```

代码实际遵循的规则：

- 仅含空格的名称视为未设置。
- 名称先按完全一致匹配，然后忽略大小写和首尾空格再匹配。
- 若 `preview_device` 与主输出实际打开的设备为同一设备（按名称判断），则该设备只打开一次。此时试听与主输出共用。
- 在独立设备上播放的试听从不进入玩家的音频流。它们也不会被闪避（ducking），且不受总线（bus）推子的影响。

> **注意：** 设备名称是主机系统自身的字符串。在一种操作系统上保存的名称，在另一种操作系统上并不存在。在某些主机上，两台设备可能同名；此时以第一台为准。

已配置但不在列表中的设备仍会显示在其下拉列表中，并带有后缀 `(not connected)`。配置会被保留，因此只需重新插入设备并点击 `Refresh devices` 即可。

`Refresh devices` 会在后台线程中重新读取设备列表（期间按钮显示 `Refreshing...`，因此控制台不会因休眠中的蓝牙设备而卡住）。扫描结束后，若有助于恢复，Bardwave 会重新应用已保存的选择：已配置的设备存在但未被使用、此前失败的试听设备已出现，或者输出失败或没有设备，而设备实际存在。

<a id="fallback-and-error-messages"></a>

### 回退与错误信息

主输出按以下顺序打开：

1. 列表中找到时，使用已配置的 `output_device`。
2. 系统默认设备。

若指定的设备不存在，音频引擎会使用系统默认设备，并予以说明。若找到了指定设备但无法打开，音频引擎同样会尝试系统默认设备。状态行会在括号中显示原因，例如：

```text
Active: Built-in Output - 48000 Hz, 2 ch (output device "Bluetooth Speaker" not found; using the system default)
```

```text
Active: Built-in Output - 48000 Hz, 2 ch (could not open "USB Headphones" (<error>); using the system default)
```

若完全无法打开任何设备（没有默认设备，或无法构建或启动音频流），音频引擎会在没有声卡的情况下继续运行，即无头模式（headless）。听众仍能接收音频流，但桌面上没有任何声音播放。此时状态行显示 `Error: no output device opened (network only)`，后接括号中的原因，例如 `(local output unavailable: <error>)`。

若音频流在运行中出错（例如设备被拔出），主输出会切换到 `Error: <message>` 状态，且不会自行恢复。请重新插入设备并点击 `Refresh devices`，或重新选择设备。

状态行含义如下：

| 状态行 | 含义 |
|---|---|
| `Active: <device> - <rate> Hz, <ch> ch` | 输出正在运行。使用回退或格式转换时，会在括号中添加原因。 |
| `Output not started` | 尚未打开任何输出。 |
| `Error: no output device opened (network only)` | 未打开任何声卡。服务器仍会向听众推送音频流。 |
| `Error: <message>` | 输出失败。消息即为原因。 |
| `Previews play on the main output` | 未使用独立的试听设备。 |
| `Active: <device> - <rate> Hz, <ch> ch`（位于 `Preview` 下方） | 试听在其独立设备上播放。 |
| `Error: preview device "<name>" not found; previews play on the main output` | 已配置的试听设备不存在。试听回退到主输出。 |
| `Error: could not open the preview device: <error>; previews play on the main output` | 试听设备存在但无法打开。同样回退到主输出。 |
| `Error: could not start the preview stream: <error>; previews play on the main output` | 试听音频流已构建但未能启动。同样回退到主输出。 |
| `Error: <error>; previews play on the main output` | 试听音频流在运行中出错（例如耳机被拔出）。试听切回主输出。 |

即使主输出没有设备（无头模式），也会尝试使用试听设备。

你可能还会看到以下消息：

| 消息 | 位置 | 含义 |
|---|---|---|
| `<name> (not connected)` | 下拉列表条目 | 已配置的设备不在列表中。 |
| 区块下方的 `Error: ...` | 音频部分 | 设备扫描失败（例如 `could not list output devices: ...`），或无法应用设备更改（例如 `the audio engine is not running`）。 |
| `Refreshing...` | 按钮 | 设备扫描正在进行。 |

<a id="buffer-size-sample-rate-and-channels"></a>

### 缓冲区大小、采样率与声道

这三个配置项不在下拉列表中。该区块仅以一行显示它们，例如 `buffer: 960 frames (20.0 ms) - 48000 Hz - 2 ch`。当 `buffer_size = 0` 时，该行以 `buffer: driver default` 开头。

| 配置项 | 默认值 | 有效值 | 说明 |
|---|---|---|---|
| `sample_rate` | `48000` | `8000` 到 `192000` | 超出范围的值会被记入日志，并替换为 `48000`。 |
| `channels` | `2` | `1` 到 `8` | 超出范围的值会被记入日志，并替换为 `2`。`1` 会下混为单声道。声道数超过 2 时，额外的声道获得左、右声道的单声道下混。 |
| `buffer_size` | `960` | `0` 或 `64` 到 `8192` | 每个设备缓冲区的帧数。`0` 表示由驱动程序决定。超出范围的值会被记入日志，并替换为 `480`（48 kHz 时为 10 ms），而不是 `960`。 |

```toml
[audio]
sample_rate = 48000
channels = 2
buffer_size = 960   # 20 ms at 48 kHz
```

这些配置项仅决定桌面声卡的打开方式。混音、网络音频流和游戏场次录制始终为 48 kHz 立体声。

这些值在应用启动时读取。更换设备或之后编辑 `config.toml`，都不会重新读取它们。下拉列表下方的信息行显示配置中原样写入的值，即使超出范围的值已被替换，也是如此。

音频引擎按以下顺序尝试打开设备：

1. 以 32 位浮点打开已配置的采样率和声道数。
2. 混音格式：48 kHz 立体声，32 位浮点。
3. 其他采样率的立体声 32 位浮点（设备默认采样率适用时使用该值，否则使用最接近 48 kHz 的采样率），并在音频回调中进行重采样。
4. 设备自身的默认格式（任意采样率、声道数和采样格式），并进行重采样与转换。

当设备无法运行已配置的采样率和声道数，或混音必须转换时，状态行会显示原因，例如 `device has no 48 kHz stereo f32 mode; using 44100 Hz stereo f32 and resampling`。若设备拒绝固定的 `buffer_size`，音频引擎会以设备默认缓冲区大小重试（控制台中无消息，仅写入日志）。

> **注意：** 音频引擎源代码中的注释仍提到默认缓冲区为 10 ms。已配置的默认值为 `960` 帧，即 48 kHz 下的 20 ms。10 ms 值（`480` 帧）仅作为无效 `buffer_size` 的回退值。

<a id="session-recording"></a>

## 游戏场次录制

顶栏中的 **Rec**（录制）按钮可将游戏场次录制为 WAV 文件。其旁边的 **folder**（文件夹）按钮用于选择录音文件夹。

| 控件 | 文字或提示 | 行为 |
|---|---|---|
| REC 按钮，空闲时 | `Rec`（提示：`Record the mix the players hear to a WAV file`） | 开始录制。 |
| REC 按钮，录制中 | `Stop rec`，栏较窄时为 `Stop`（提示：`Stop and save the recording`） | 停止录制并保存文件。 |
| 文件夹按钮 | 提示：`Recordings folder: <path>`，后接 `Click to change (applies to the next recording)` | 打开标题为 `Recordings folder` 的文件夹选择器。录制期间点击无效。窗口较窄时，栏会优先隐藏此按钮。 |
| 读数 | `HH:MM:SS  ·  <size>` | 录制期间显示已用时间和磁盘占用大小（所有分段，包括文件头）。栏中空间足够时显示。 |
| `<n> lost` | 琥珀色文字 | 因磁盘写入速度跟不上，帧被丢弃。 |

录制开始过程中或上一次录制仍在收尾时，REC 按钮不可用。开始和停止在短暂的后台线程中执行，因此控制台保持响应。点击 `Stop rec` 后，文件由写入线程完成收尾。随后控制台显示 `Saved <file name> (<duration>, <size>)`；若录制被分段，后接 ` in <n> files`；若有帧被丢弃，则显示 `; <n> frames were lost (disk too slow)`。大小以 1024 为基数的单位显示（`KB`、`MB`、`GB`）。

<a id="what-is-recorded"></a>

### 录制内容

录制的内容为玩家所听到的混音：

- 它在混音器的主音量（master volume）和限幅器之后采集，即混音送往网络广播器的位置。
- 无论声卡采样率为多少，它都是 48 kHz 立体声，且与用于播放的设备无关。
- 声音试听不包含在录制中。试听走独立的路径播放，从不进入玩家的音频流。
- 录制永远不会被音频回调阻塞。若写入器因磁盘速度慢而落后，整个缓冲区会被丢弃，并计为丢失的帧。
- 在没有任何声音播放时，写入器会根据挂钟时间补入静音。文件保留真实的游戏场次时间。

录制在服务器停止且无人连接时是否继续运行，尚未在真实场次中测试（未验证）。录制器在服务器启动前就已连接到混音器输出。

<a id="where-files-go-and-how-they-are-named"></a>

### 文件保存位置与命名方式

默认文件夹为正在运行的配置方案的数据目录中的 `recordings/`。数据目录在[配置方案](#profiles)中有说明。对于默认配置方案，数据目录即根数据目录。

文件以录制开始时的 UTC 时间命名：

```text
bardwave-20261119-203015.wav
```

- 若文件名已被占用，则追加 `-2`、`-3` 等后缀（最多到 `-999`）。录制文件从不会被覆盖。
- 需要多个文件的录制会以 `bardwave-20261119-203015_part2.wav`、`_part3.wav` 等继续。添加 `-2` 后，各分段随之使用该名称，例如 `bardwave-20261119-203015-2_part2.wav`。

要更改文件夹，请点击文件夹按钮。所选文件夹会保存到正在运行的配置方案配置中的 `[gui] recordings_dir`。它对下一次录制生效。空值视为未设置。若文件夹不存在，则会自动创建。

```toml
[gui]
recordings_dir = "/Users/you/Recordings"
```

<a id="size-limits-and-splitting"></a>

### 大小限制与分段

| 设置项 | 值 | 作用 |
|---|---|---|
| 分段大小 | 每个文件的音频数据 `1800000000` 字节（约 1.8 GB） | 录制继续写入下一个 `_partN.wav` 文件。 |
| 大小警告 | 单个文件超过 `1500000000` 字节（约 1.5 GB） | 显示 `Large recording: <x> GB in this file; it continues in a new file at <y> GB`。 |
| 最小可用空间 | 200 MB（`200 * 1024 * 1024` 字节） | 可用空间低于此值时，录制不会开始。 |
| 低空间警告 | 按当前格式计算，可用空间不足一小时；在录制开始时检查 | 显示 `Low disk space: <n> MB free, about <n> min of recording`。 |
| 文件头更新 | 约每秒一次 | 因崩溃而遗留的文件，在最后一次更新之前的部分是有效的 WAV。 |

可用空间检查在 Unix 系统（macOS、Linux）上有效。在其他系统（Windows）上会跳过该检查，磁盘写满时录制将因写入错误而结束（Windows 上未验证）。

WAV 格式单个文件不能超过 4 GB，因此分段大小上限设在略低于该值（约 4.29 GB）。默认分段大小远低于此上限。

以 48 kHz 立体声计算的大致文件大小：

| 格式 | 每小时 | 每小时分段数 |
|---|---|---|
| 16 位 PCM（默认） | 约 691 MB | 不足一个分段 |
| 32 位浮点 | 约 1.38 GB | 约 0.8 个分段 |

<a id="recording-settings"></a>

### 录制设置

格式在 `config.toml` 中设置。控制台中没有对应的控件。

```toml
[gui]
record_float32 = false   # false: 16-bit PCM (default); true: 32-bit float
```

<a id="recording-troubleshooting"></a>

### 录制故障排查

| 现象 | 原因 | 处理方法 |
|---|---|---|
| `Cannot start recording: not enough free disk space: <n> MB free, at least 200 MB needed` | 可用磁盘空间过少。 | 释放空间，或选择其他文件夹。 |
| `Cannot start recording: cannot create the recordings folder <path>: ...` | 无法创建文件夹。 | 使用文件夹按钮选择一个可写入的文件夹。 |
| `Cannot start recording: cannot create <path>: ...` | 无法创建第一个文件。 | 检查文件夹是否可写。 |
| `Cannot start recording: a recording is already in progress` | 录制已在进行中。 | 先停止当前录制。 |
| `Low disk space: ...` | 可用空间不足一小时。 | 释放空间，或尽快停止录制。 |
| 琥珀色的 `<n> lost`，或 `frames were lost (disk too slow)` | 磁盘速度跟不上。 | 录制到更快的磁盘。 |
| `Recording stopped: ...` | 写入失败。 | 检查磁盘及消息内容。到该处为止的文件会被保留。 |

<a id="profiles"></a>

## 配置方案

配置方案是一个独立的战役，拥有自己的 `config.toml` 和声音库。默认配置方案即根数据目录。命名配置方案位于其自己的文件夹中：

```text
<root>/                       default profile: config.toml, bardwave-library/
<root>/profiles/<name>/       named profile: config.toml, bardwave-library/
<root>/recent.toml            shared: recently used profiles
<root>/bardwave.log           shared: log (written when [logging] file_logging = true)
<root>/tools/                 shared: downloaded tools
```

根目录由 `src/paths.rs` 中的平台规则决定。简而言之：若可执行文件所在文件夹已包含 `config.toml` 或 `portable.flag` 文件，或该文件夹可写（且不位于 macOS 的 `.app` 应用包内），则根目录即该文件夹。否则，macOS 上为 `~/Library/Application Support/Bardwave`，Windows 上为 `%APPDATA%\Bardwave`，Linux 上为 `$XDG_DATA_HOME/bardwave`（或 `~/.local/share/bardwave`）。配置、声音库和录音分别属于各个配置方案。日志、工具和 `recent.toml` 由所有配置方案共享。

<a id="starting-with-a-profile"></a>

### 以配置方案启动

使用 `--profile` 选项启动 Bardwave：

```bash
soundshared --profile "Campaign A"
soundshared --profile=Campaign-A
soundshared --profile default
```

使用 cargo 构建时，可执行文件名为 `soundshared`（例如 `cargo run --release -- --profile "Campaign A"`）；Linux 的 `.deb` 包将其安装为 `bardwave`。`--profile default` 与不加该选项相同。该选项同样可与 `--server-only` 一起使用。

名称规则：

| 规则 | 被拒绝的名称示例 |
|---|---|
| 1 到 32 个字符（首尾空格会被去除） | 空名称，或 33 个字符的名称 |
| 字母、数字、空格、`-` 和 `_`（任何字母表中的字母均可） | `Campaign/A`、`camp.a` |
| 不能为 `default`（不区分大小写） | `Default` |
| 不能为 Windows 保留名称 | `CON`、`PRN`、`AUX`、`NUL`、`COM1` 到 `COM9`、`LPT1` 到 `LPT9` |
| 唯一，不区分大小写 | 当 `Campaign A` 已存在时的 `campaign a` |

若名称无效，Bardwave 不会启动。它会向 stderr 输出 `Error: invalid profile name: ...`，并以退出码 2 退出。除非使用了 `--server-only`，否则还会显示一个消息框。`--profile` 后没有值时，输出 `--profile needs a profile name`。当配置方案文件夹无法创建时，同样使用退出码 2。该选项重复出现时，以最后一个为准。

尚不存在的配置方案会由 `--profile` 创建（其文件夹在启动时生成）。其首次启动使用默认设置。

<a id="switching-profiles"></a>

### 切换配置方案

点击顶栏中的配置方案名称（栏中有足够空间时，其前方会绘制 `PROFILE` 标题）。命名配置方案的窗口标题以 ` — <name>` 结尾。菜单标题为 `Switch to (restarts Bardwave)`。

1. 从列表中选择一个配置方案。默认配置方案显示为 `Default`。正在运行的配置方案标有 `(running)`，无法选择。
2. 在 **Switch Profile**（切换配置方案）对话框中阅读文字：`Switch to "<name>"? The server and all sounds stop, the current profile is saved.`
3. 点击 `Switch`（或 `Cancel`）。

Bardwave 会保存当前配置方案，关闭窗口，停止服务器和音频，然后以 `--profile <name>`（默认配置方案则为 `--profile default`）启动一个新进程。新进程与当前进程分离。若无法启动，错误会被记入日志，Bardwave 直接退出。切换功能仅存在于控制台窗口中；`--server-only` 没有菜单。

<a id="managing-profiles"></a>

### 管理配置方案

配置方案菜单包含以下项目：

| 项目 | 结果 |
|---|---|
| `New profile…` | 打开 **New Profile**（新建配置方案）。 |
| `Manage profiles…` | 打开 **Profiles**（配置方案列表），其中每个其他配置方案都有 `Rename` 和 `Delete` 按钮。默认配置方案和正在运行的配置方案没有按钮。该对话框还包含 `New profile…` 和 `Close`。 |
| `Open data folder` | 在文件管理器中打开正在运行的配置方案的数据目录。 |

**New Profile** 包含一个 `Profile name` 输入框（提示文字为 `Campaign name`）、复选框 `Copy the current library`，以及按钮 `Create`。输入框下方显示 `Letters, digits, space, - and _`，或名称被拒绝的原因。复制选项：

- 复制正在运行的配置方案的 `config.toml` 和 `bardwave-library/`（不复制其他内容，因此不会复制录音；符号链接会被跳过）；
- 复制整个配置，因此服务器端口等其他设置也会一并复制；
- 改写复制后配置中的声音库路径，使其指向新文件夹；
- 在后台运行，对话框显示 `Copying the library…`。复制结束前无法关闭该对话框。

若复制失败，部分复制的配置方案会被再次删除。未选择复制选项时，将创建一个空文件夹。点击 `Create` 后，**Switch Profile** 对话框会打开，并显示提示 `Profile created`。在点击 `Switch` 之前，新配置方案不会启动。

**Rename Profile**（重命名配置方案）会重命名文件夹，并按相同规则接受新名称（输入框文字为 `New name`），同时更改其配置中的声音库路径。按钮为 `Rename`。允许仅更改字母大小写。重命名后，列表显示 `Renamed <old> to <new>`。

**Delete Profile**（删除配置方案）会永久删除配置方案文件夹。没有回收站。

- 空配置方案（即没有任何文件的文件夹）会直接删除，不再询问。对话框显示 `This profile is empty.`
- 非空配置方案需要在输入框 `Type "<name>" to confirm` 中输入与其完全一致的名称。对话框显示 `Its config and library are deleted for good. There is no trash.` 在名称匹配之前，按钮保持不可用（区分大小写）。
- 按钮为 `Delete`。删除后，列表显示 `Deleted <name>`。

> **警告：** 删除配置方案会移除其整个文件夹：配置、声音库以及默认的 `recordings/` 文件夹。此操作无法撤销。如需备份，请先使用 `Open data folder` 并复制该文件夹。

正在运行的配置方案无法重命名或删除。控制台不提供相关按钮，代码也会以 `The running profile cannot be changed` 拒绝操作。请先切换到其他配置方案。

<a id="recent-profiles"></a>

### 最近使用的配置方案

根文件夹中的 `recent.toml` 最多保存 8 个配置方案名称，最新的在前。每次启动 Bardwave（包括 `--server-only`）时，正在运行的配置方案都会被移到最前面。

- 配置方案菜单先列出最近使用的配置方案，然后列出 `Default`（若不在其中），最后按字母顺序列出其他配置方案。
- 已不存在的配置方案不会显示。
- 重命名或删除配置方案时，会从最近使用列表中移除其旧名称。
- 缺失或无法读取的 `recent.toml` 视为空列表。

```toml
# <root>/recent.toml
recent = ["Campaign A", "default"]
```

默认配置方案以 `default` 形式保存。

<a id="update-check"></a>

## 检查更新

此检查用于查找较新的发行版。它不下载任何内容，也不安装任何内容。该功能位于服务器面板的 **Updates** 区块中。

| 控件 | 文字 | 行为 |
|---|---|---|
| 按钮 | `Check for updates`（提示：`Asks GitHub for the newest release. Nothing else is sent or downloaded.`） | 立即执行一次检查。检查进行期间不可用。 |
| 复选框 | `Check once a day at start`（提示：`Off by default: Bardwave makes no network requests on its own.`） | 启用自动检查。 |
| 结果行 | `Checking...`、`Up to date (v<version>)`、`Update available: v<version>` | 显示检查结果。 |
| 链接 | `Open release page` | 在浏览器中打开发布页面。仅在有可用更新时显示。 |

有可用更新时，状态栏还会显示链接 `Update available: v<version>`，点击后打开同一页面。

失败信息以琥珀色显示：

| 消息 | 原因 |
|---|---|
| `Network error: could not reach the update server` | 无连接、DNS 或 TLS 失败，或 8 秒超时。 |
| `Update server answered HTTP <code>` | 服务器返回了错误状态（例如速率限制）。 |
| `Update server sent an oversized reply` | 回复超过 256 KiB。 |
| `Update server sent an unreadable reply` | 回复不是发行版文档。 |
| `Could not start the update check` | 无法启动工作线程。 |

该检查默认关闭：

```toml
[gui]
check_updates = false      # daily check at start; default false
last_update_check = 0      # unix time of the last check; written by Bardwave
```

当 `check_updates` 开启时，若从未检查过，或距离 `last_update_check` 已至少过去 24 小时，Bardwave 会在每次控制台启动时检查一次（系统时钟倒退也视为已到期）。手动检查同样会更新 `last_update_check`。无论每日检查是否开启，手动按钮都可以使用。只会提示稳定版发行版：草稿和预发行版会被忽略；若构建自身的版本号无法解析为 `major.minor.patch`，则永远不会被提示更新。

发送的内容：向 GitHub 的“最新发行版”API（`https://api.github.com/repos/adamantida/Bardwave/releases/latest`）发出一次 `GET` 请求。代码仅设置两个请求头：

| 请求头 | 值 |
|---|---|
| `User-Agent` | `Bardwave/<version>` |
| `Accept` | `application/vnd.github+json` |

请求不携带 Cookie、授权信息或 referrer。HTTP 库会添加其自身的传输头（例如 `Host`），与任何网页请求一样，GitHub 可以看到发出请求的 IP 地址。请求在 8 秒后超时。超过 256 KiB 的回复会被拒绝。检查不发送设置、声音库内容或配置方案名称。发布页面链接由 Bardwave 根据版本号生成，从不取自回复内容。

> **注意：** 请求及解析已针对本地测试监听器进行过测试。从发行版构建向 GitHub 发出的真实请求，本文档中未验证。
