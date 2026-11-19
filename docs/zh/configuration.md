# 配置参考

本页列出 Bardwave 读取的 `config.toml` 配置项、命令行选项和环境变量。默认值与取值范围来自 `src/config.rs`、`src/paths.rs`、`src/profiles.rs`、`src/main.rs` 以及 `src/gui/groups.rs` 中的声音库加载器。未在真实硬件上检验的行为标记为 **（未验证）**。

HTTPS 设置参见 [HTTPS（安全连接）](./https.md)。相关页面：[场景、音乐、快捷键与定时器](./scenes-music-hotkeys.md)、[声音库](./soundboard-library.md)、[网络、玩家与主持人远程面板](./network-remote.md)、[音频设备、录制、配置方案与更新](./audio-session-tools.md)、[故障排查、常见问题（FAQ）与安全](./troubleshooting-security.md)。

## 目录

- [文件位置](#file-location)
- [文件格式与迁移](#file-format-and-migration)
- [命令行选项](#command-line-flags)
- [环境变量](#environment-variables)
- [\[server\]](#server)
- [\[audio\]](#audio)
- [\[logging\]](#logging)
- [\[gui\]](#gui)
- [\[\[hotkeys\]\]](#hotkeys)
- [\[\[timers\]\]](#timers)
- [\[library\]](#library)
- [完整示例](#complete-example)

<a id="file-location"></a>
## 文件位置

满足前三行中任一条件时，根数据目录即为程序所在文件夹；否则为用户专属文件夹：

| 情形 | 目录 |
|---|---|
| 可执行文件旁存在 `portable.flag` | 程序所在文件夹 |
| 可执行文件旁存在 `config.toml`（Windows：仅当该文件夹可写时） | 程序所在文件夹 |
| 程序所在文件夹可写，且不在 macOS `.app` 内 | 程序所在文件夹 |
| 其他情况，macOS | `~/Library/Application Support/Bardwave` |
| 其他情况，Windows | `%APPDATA%\Bardwave` |
| 其他情况，Linux 及其他 Unix | `$XDG_DATA_HOME/bardwave` 或 `~/.local/share/bardwave` |

如果无法确定用户专属文件夹（缺少 `HOME`、`USERPROFILE` 或 `APPDATA`），或无法创建该文件夹，则使用程序所在文件夹。空的环境变量视为未设置。

**便携模式：** 在可执行文件旁创建一个名为 `portable.flag` 的空文件。只检查该文件是否存在。未在 Windows 和 Linux 上测试安装 **（未验证）**。

使用 `--profile <name>` 时，文件的位置会有所不同：

| 路径（根目录下） | 内容 |
|---|---|
| `config.toml`、`bardwave-library/` | 默认配置方案（profile） |
| `profiles/<name>/config.toml`、`profiles/<name>/bardwave-library/` | 命名配置方案 |
| `bardwave.log`、`tools/`、`recent.toml` | 所有配置方案共用 |

录音保存到 `recordings/`，自签名 HTTPS 证书保存到 `tls/`，两者都位于配置方案的数据目录中。`bardwave-library/` 保存 Bardwave 下载的音频；`tools/` 保存其下载的辅助工具。命名配置方案的文件夹在首次使用时创建。

配置方案名称会去除首尾空格，长度为 1–32 个字符：字母、数字、空格、`-`、`_`。名称 `default`（不区分大小写）表示默认配置方案，普通配置方案不能使用该名称。Windows 设备名（`CON`、`PRN`、`AUX`、`NUL`、`COM1`–`COM9`、`LPT1`–`LPT9`）会被拒绝。在控制台中切换配置方案会重新启动 Bardwave，并切换到另一个配置方案。

<a id="file-format-and-migration"></a>
## 文件格式与迁移

`config.toml` 为 TOML 格式。所有配置项均为可选。缺失的配置项使用下文所示的默认值。整数类型的配置项为无符号数：负数或过大的数值（例如 `port = 70000`）会导致解析错误。

- 文件不存在时全部使用默认值。文件会在首次启动时创建。
- 未知配置项在加载时被忽略，保存时被删除。
- 配置文件无法解析（或无法读取）时，启动会中止并以退出码 1 退出。对话框显示 `config.toml could not be read`；使用 `--server-only` 时没有对话框，错误输出到 stderr。不会回退到默认值。

> **警告：** Bardwave 每次保存时都会按自己的格式重写 `config.toml`，包括首次启动（此时会保存访问密钥）和对声音库的编辑。注释会丢失，包括随附示例文件中的注释。大多数配置项即使为默认值也会完整写出。未设置时，可选配置项和空列表不会写出：`output_device`、`preview_device`、`recordings_dir`、`tls_cert`、`tls_key`、`tags`、`pan = 0`、`ui_scale = 1.0`、`record_float32 = false`、`last_update_check = 0`、`onboarding_done = false`，以及空的 `[[hotkeys]]`、`[[timers]]`、场景和播放列表。内联表（例如 `action = { ... }` 和 `trigger = { ... }`）会以 `[hotkeys.action]` 和 `[timers.trigger]` 表的形式写回（含义相同）。浮点数可能多出若干位小数（`0.8` 保存为 `0.800000011920929`）。请保留手写文件的副本。

旧数据迁移：

- 没有 `id` 的声音、场景和播放列表会在加载时获得 UUID，并在下次保存时写入。v1.4.0 版本的配置没有 id，就是这样升级的。
- 手写的 id（例如 `"rain"`）会变为 UUID。场景条目、发射器（emitter，按随机间隔播放声音的声源）、场景的 `playlist`、播放列表的 `items` 以及 `[[hotkeys]]` 中的引用会相应改写。`[[timers]]` 中的引用不会被改写：此处请使用 UUID。
- 重复的 id 会获得新的 UUID。最先使用该 id 的条目保留原 id。
- 缺失的 `access_token` 或 `gm_token` 会在启动时生成并保存。
- 旧版配置中缺失的节或配置项（`[gui]`、`[library]`、`output_device`、`tls` 等）使用默认值，因此旧文件无需修改即可继续工作。已有声音的配置会在首次启动控制台时标记为 `onboarding_done = true`。
- 现有安装保持数据原位：可执行文件旁的 `config.toml` 会使程序所在文件夹继续作为数据目录（参见 [文件位置](#file-location)）。不会移动任何内容。

<a id="command-line-flags"></a>
## 命令行选项

其他参数会被忽略。没有 `--help` 或 `--version`。

| 选项 | 作用 |
|---|---|
| `--server-only` | 以无窗口方式运行。加载已保存的声音库，启动网络服务器，并在日志中输出听众链接和控制页面链接（默认 `info` 级别）。按 Ctrl+C 停止。此模式下不运行 `[[hotkeys]]` 和 `[[timers]]`。后台测量的响度（`normalize`）会保存到 `config.toml`。 |
| `--profile <name>` | 使用该配置方案。`--profile=<name>` 同样有效。`default` 表示默认配置方案。多次出现时，以最后一次为准。 |

缺少值或名称无效的 `--profile` 会输出 `Error: ...` 并以退出码 2 退出。配置方案文件夹无法创建时也会如此。不使用 `--server-only` 时，该消息还会显示在对话框中。

```bash
./bardwave --server-only
./bardwave --server-only --profile "Camp 1"
```

使用 `--server-only` 时，日志会以 `/control?g=<gm_token from config.toml>` 的形式显示远程控制链接。令牌本身从不写入日志。

<a id="environment-variables"></a>
## 环境变量

| 变量 | 作用 |
|---|---|
| `RUST_LOG` | 若设置为有效的过滤器，则替代 `[logging] level`。使用 `tracing` 过滤器语法。未设置或值无效时，回退到 `[logging] level`。 |
| `SOUNDSHARED_REDUCED_MOTION` | `1`、`true`、`yes`、`on` 关闭装饰性动效；`0`、`false`、`no`、`off` 开启装饰性动效。不区分大小写。其他值（包括空值）会被忽略。仅在本次运行期间覆盖 `[gui] reduce_motion`。 |
| `HOME`、`USERPROFILE`、`APPDATA`、`XDG_DATA_HOME` | 仅用于确定用户专属数据目录。 |

<a id="server"></a>
## [server]

| 配置项 | 类型 | 默认值 | 有效值 | 作用 |
|---|---|---|---|---|
| `port` | 整数 | `8080` | 1–65535 | HTTP 和 WebSocket 端口。控制台会拒绝 0。文件中的 `0` 未经检验 **（未验证）**。 |
| `host` | 字符串 | `"0.0.0.0"` | IP 地址 | 绑定地址。`0.0.0.0` 表示所有 IPv4 接口。控制台仅接受 IP 字面量。文件中不是 IP 地址的值会在启动时替换为 `0.0.0.0`。 |
| `upnp` | 布尔值 | `true` | | 请求路由器（UPnP IGD）转发该端口。 |
| `access_token` | 字符串 | 自动生成 | 任意值；`""` 表示开放 | 听众链接 `/?k=<key>` 中的访问密钥。缺失时会在启动时生成并保存：16 个字符，由 `a-z` 和 `0-9` 组成。 |
| `gm_token` | 字符串 | 自动生成 | 任意值；`""` 表示仅限本机 | 控制页面 `/control?g=<key>` 的主持人（GM）令牌。缺失时会自动生成。`""` 使页面仅在本机可用。 |
| `tls` | 布尔值 | `false` | | HTTPS 和 `wss://` 共用同一端口。下次启动服务器时生效。 |
| `tls_cert` | 字符串（路径） | 无 | PEM 证书链 | 自备证书。须与 `tls_key` 同时设置，或都不设置（此时会在 `tls/` 中生成自签名证书）。空字符串视为未设置。 |
| `tls_key` | 字符串（路径） | 无 | PEM 私钥 | `tls_cert` 对应的私钥。 |

在控制台的“CONFIGURATION”（配置）部分中，`port`（PORT）和 `host`（BIND ADDRESS）仅在服务器停止时可编辑。访问密钥显示在“SERVER”（服务器）面板的“ACCESS KEY”（访问密钥）中，主持人令牌则位于“GM panel link”（主持人面板链接）下方（**Create GM key**（创建主持人令牌）、**New key**（新令牌）、**Replace key**（更换令牌））。

设置 `tls = true` 时，若 `tls_cert` 和 `tls_key` 只设置了其中一个，启动服务器会失败，并报错 `tls_cert and tls_key must be set together`，服务器不会启动。

> **警告：** `access_token = ""` 会让任何能连接到该端口的人都可以收听。仅应在可信网络中使用。

> **注意：** `gm_token` 默认生成，因此远程控制处于开启状态。持有 `?g=` 链接的任何人都可以按下主持人按钮。设置 `gm_token = ""` 可使页面仅在本机使用。

<a id="audio"></a>
## [audio]

| 配置项 | 类型 | 默认值 | 有效值 | 作用 |
|---|---|---|---|---|
| `buffer_size` | 整数 | `960` | `0` 或 64–8192 | 声卡缓冲区大小，单位为帧。960 即 48 kHz 下的 20 ms。`0` 为驱动默认值。其他值会发出警告，并改用 480（10 ms）。 |
| `sample_rate` | 整数 | `48000` | 8000–192000 | 向声卡请求的采样率。发往玩家的音频流始终为 48 kHz。超出范围时会发出警告，并改用 48000。 |
| `channels` | 整数 | `2` | 1–8 | 声卡声道数。1 为单声道下混；多于 2 时，会在额外声道上添加居中混音。超出范围时会发出警告，并改用 2。 |
| `master_volume` | 浮点数 | `0.8` | 0.0–1.0 | 启动时的主音量。**超出此范围的值会使启动中止并报错**（无对话框；错误输出到 stderr）。 |
| `normalize` | 布尔值 | `false` | | 测量 `gain_db` 为 `0.0` 的声音的响度（EBU R128），并将修正值写入 `gain_db`。从磁盘流式读取的声音不会测量。控制台：**NORMALIZE LOUDNESS**（响度归一化）。 |
| `normalize_target_lufs` | 浮点数 | `-18.0` | 在 -60 到 0 范围内生效 | 响度归一化（normalization）的目标值。控制台的 **Target**（目标响度）输入框提供 -36 至 -8 LUFS。不是数字的值回退为 -18。 |
| `duck_amount` | 浮点数 | `0.3` | 在 0.0–1.0 范围内生效 | 被闪避（ducking）的总线（bus）的线性音量。`0.3` 约为 -10 dB。`1.0` 关闭闪避，`0.0` 使其静音。控制台：**Duck settings**（闪避设置）、**Level**（音量）。 |
| `duck_attack_ms` | 整数 | `200` | 毫秒 | 降低音量的淡变时间。控制台允许 0–10000。 |
| `duck_release_ms` | 整数 | `800` | 毫秒 | 恢复音量的淡变时间。控制台允许 0–30000。 |
| `duck_buses` | 字符串数组 | `["music", "ambience"]` | `music`、`ambience`、`sfx` | 闪避期间降低音量的总线。控制台：**Ducks**（闪避）复选框。 |
| `stream_threshold_secs` | 整数 | `60` | 秒；`0` 表示关闭 | 超过此时长的文件会边播放边从磁盘读取。 |
| `output_device` | 字符串 | 无 | 设备名称 | 桌面输出设备。找不到该设备时回退到系统默认设备，控制台会给出提示。控制台：**AUDIO OUTPUT**（音频输出）、**Output**（主输出）。 |
| `preview_device` | 字符串 | 无 | 设备名称 | 仅用于试听（preview）的设备。未设置时使用主输出。试听的声音不会发送给玩家。控制台：**Preview**（试听设备）。 |

**AUDIO OUTPUT** 部分中的设备更改会立即应用并保存。缓冲区大小、采样率和声道数在此处只读显示。空白的设备名称视为未设置。

<a id="logging"></a>
## [logging]

| 配置项 | 类型 | 默认值 | 有效值 | 作用 |
|---|---|---|---|---|
| `level` | 字符串 | `"info"` | `trace`、`debug`、`info`、`warn`、`error` | 日志级别。`RUST_LOG` 会覆盖此设置。 |
| `file_logging` | 布尔值 | `false` | | 同时写入 `bardwave.log`（追加模式）。若有终端，日志也始终输出到终端。 |

日志文件为根目录下的 `bardwave.log`（参见 [文件位置](#file-location)），由所有配置方案共用。无法打开时会跳过文件日志，也不会给出任何提示。

<a id="gui"></a>
## [gui]

| 配置项 | 类型 | 默认值 | 有效值 | 作用 |
|---|---|---|---|---|
| `reduce_motion` | 布尔值 | `false` | | 停止实时指示点的脉冲动画和背景扫描线。启动时读取。 |
| `global_hotkeys` | 布尔值 | `false` | | `[[hotkeys]]` 在窗口没有焦点时也会触发 **（macOS 权限未验证）**。控制台：“Hotkeys”（快捷键）对话框中的 **GLOBAL HOTKEYS (WHEN THE WINDOW IS NOT FOCUSED)**（窗口没有焦点时的全局快捷键）。 |
| `recordings_dir` | 字符串（路径） | 无 | 文件夹 | 用于录音的文件夹（**Rec**（录制）按钮）。未设置或为空时，使用配置方案中的 `recordings/`。**Rec** 旁的文件夹按钮可设置该项。 |
| `record_float32` | 布尔值 | `false` | | 32 位浮点 WAV，而非 16 位 PCM（控制台中无对应控件；仅通过配置设置）。 |
| `check_updates` | 布尔值 | `false` | | 每天在启动时检查一次 GitHub（距上次检查已满 24 小时时）。控制台：**Check once a day at start**（启动时每天检查一次）。无论此设置如何，**Check for updates**（检查更新）都可用。 |
| `last_update_check` | 整数 | `0` | Unix 秒数 | 由 Bardwave 写入。为 0 时不写入。 |
| `ui_scale` | 浮点数 | `1.0` | 0.8–1.6 | 界面缩放，实时生效。**Scale**（界面缩放）滑块以 0.05 为步长移动。**Reset**（重置）可恢复到 100%。超出范围的值会限制在该范围内；不是数字的值按 1.0 读取。 |
| `onboarding_done` | 布尔值 | `false` | | 完成或关闭 **Getting started**（首次运行向导）时设置，配置中已有声音时也会设置。Help > **Getting started** 可再次打开该向导。 |

<a id="hotkeys"></a>
## [[hotkeys]]

绑定位于文件顶层，而不在 `[library]` 中。仅控制台窗口使用这些绑定。

- `key`（必填）：修饰键与一个按键，以 `+` 连接，例如 `Ctrl+Shift+A` 或 `F9`。修饰键为 `Ctrl`、`Alt`、`Shift`、`Cmd`（macOS 的 Command 键，或 Windows / Super 键），别名包括 `Control`、`Option`/`Opt`、`Command`/`Super`/`Meta`/`Win`。`CmdOrCtrl` 在 macOS 上表示 `Cmd`，在其他系统上表示 `Ctrl`。按键包括字母、数字、`F1`–`F24`、`Numpad0`–`Numpad9`、`Space`、`Enter`、`Escape`、`Tab`、`Backspace`、方向键（`Up`、`Down`、`Left`、`Right`）以及类似的具名按键。不区分大小写，顺序不限。
- `action`（必填）：内联表，包含 `kind`；前三种类型还需要 `target`。

| `kind` | `target` | 控制台文字 | 作用 |
|---|---|---|---|
| `sound` | 声音 id | Play sound / stinger（播放声音 / 短音效） | 播放。短音效（stinger）只播放一次；其他声音则切换播放状态。 |
| `scene` | 场景 id | Activate scene（激活场景） | 激活该场景。 |
| `playlist` | 播放列表 id | Start playlist（开始播放列表） | 开始或重新开始播放该播放列表。 |
| `stop_all` | 无 | Stop all（全部停止） | 停止所有内容。 |
| `stop_scene` | 无 | Stop scene（停止场景） | 停止当前活动的场景。 |
| `master_up` | 无 | Master volume up（主音量调高） | 调高一步（0.05）。 |
| `master_down` | 无 | Master volume down（主音量调低） | 调低一步（0.05）。 |
| `master_mute` | 无 | Master mute（主音量静音） | 静音或取消静音。 |
| `duck_toggle` | 无 | Duck background (toggle)（背景闪避（切换）） | 按一次开始闪避，再按一次释放。 |
| `duck_hold` | 无 | Duck background (hold)（背景闪避（按住）） | 按住时闪避。 |
| `next_track` | 无 | Next track（下一曲目） | 播放列表的下一首曲目。 |
| `prev_track` | 无 | Previous track（上一曲目） | 播放列表的上一首曲目。 |
| `pause_music` | 无 | Pause / resume music（暂停 / 继续音乐） | 暂停或继续播放音乐。 |

> **注意：** 无效的绑定会保留并报告，不会删除。若两个条目使用相同的按键，则第一个生效，另一个永远不会触发。MIDI 按键（`Midi:...`）在当前版本中不会触发，参见 [场景、音乐、快捷键与定时器](./scenes-music-hotkeys.md)。

<a id="timers"></a>
## [[timers]]

定时器为场次定时器。只保存定义。它们始终以停止状态启动，并且仅在 Bardwave 控制台打开时运行（使用 `--server-only` 时不运行）。控制台：**Timers**（定时器）按钮，以及 **Start**（开始）、**Pause timers**（暂停定时器）、**Reset**（重置）和 **New timer**（新建定时器）。

| 配置项 | 类型 | 默认值 | 作用 |
|---|---|---|---|
| `id` | 字符串 | `""` | 唯一标识符。控制台会生成 UUID；手写定时器须为每个定时器指定各自的 id，因为倒计时按 id 匹配。 |
| `name` | 字符串 | `""` | “Timers”对话框中的名称（控制台中最多 60 个字符）。 |
| `trigger` | 内联表 | `{ after_ms = 600000 }` | `after_ms = N` 在“Start”后 N ms 触发一次。`every_ms = N` 表示重复触发。间隔小于 100 ms 的定时器无法运行。控制台中以分钟（0–999）和秒为单位编辑该值。 |
| `action` | 内联表 | `{ kind = "stop_all", fade_ms = 2000 }` | 触发时执行的动作（见下文）。 |
| `enabled` | 布尔值 | `true` | 为 `false` 时，定时器不会进入待触发状态。 |

触发条件小于 100 ms，或目标为空的定时器永远不会运行。

| `kind` | 字段 | 控制台文字 | 作用 |
|---|---|---|---|
| `activate_scene` | `scene` | Activate scene | 切换到该场景。 |
| `stop_scene` | 无 | Stop scene | 淡出当前活动的场景。 |
| `play_stinger` | `sound` | Play stinger（播放短音效） | 在所有内容之上播放一次该声音。 |
| `stop_all` | `fade_ms` | Stop all (fade out)（淡出并全部停止） | 淡出所有内容。文件中省略的 `fade_ms` 视为 `0`。控制台提供 0–120 s。 |
| `start_playlist` | `playlist` | Start playlist | 开始或重新开始播放该播放列表。 |
| `stop_playlist` | 无 | Stop music（停止音乐） | 淡出并停止音乐。 |
| `next_track` | 无 | Next track | 播放列表的下一首曲目。 |
| `pause_music` | 无 | Pause music（暂停音乐） | 暂停音乐。 |
| `resume_music` | 无 | Resume music（继续音乐） | 继续播放音乐。 |

<a id="library"></a>
## [library]

声音、场景和播放列表都有 UUID 格式的 `id`，引用时使用 id，绝不使用路径。控制台将 `volume` 限制在 0.0–1.0，`pan` 限制在 -1.0–1.0，`gain_db` 限制在 -48.0–24.0。加载时，`pan`、`gain_db`、场景条目的 `volume` 以及发射器的音量会限制在这些范围内（不是数字的值变为 0）。加载时不会限制声音本身的 `volume`。

加载声音库时的修复规则：

- 若分组的 `id` 不是 UUID 或与之前的分组重复，或其去除首尾空格后的 `name` 为空或超过 32 个字符，则跳过该分组。
- 若声音的 `path` 为空或与之前的声音路径重复，则跳过该声音。`group` 引用了不存在的分组时，该声音会变为未分组。
- id 不是 UUID 的场景条目、发射器声音和播放列表曲目会被丢弃。不匹配任何对象的 UUID 会保留，并作为警告报告。
- 场景的 `color` 不是 `#rgb` 或 `#rrggbb` 时会丢弃。场景的 `playlist` 不是 UUID 时也会丢弃。
- 场景和播放列表名称在控制台中限制为 64 个字符，并且不区分大小写地保持唯一。

### [[library.groups]]

| 配置项 | 类型 | 默认值 | 作用 |
|---|---|---|---|
| `id` | 字符串 | 必填 | 分组 UUID。 |
| `name` | 字符串 | 必填 | 分组名称，1–32 个字符，在控制台中创建时不区分大小写地保持唯一。 |

### [[library.sounds]]

| 配置项 | 类型 | 默认值 | 作用 |
|---|---|---|---|
| `id` | 字符串 | 自动生成 | 声音 UUID。 |
| `path` | 字符串 | 必填 | 音频文件路径。 |
| `name` | 字符串 | `""` | 显示名称。为空时显示文件名。 |
| `volume` | 浮点数 | `1.0` | 音量，0.0–1.0。 |
| `looped` | 布尔值 | `false` | 结束后重新开始播放。 |
| `group` | 字符串 | 无 | 分组 id。 |
| `tags` | 字符串数组 | `[]` | 去除首尾空格并转为小写，每个最多 32 个字符，且不重复。为空时不写入。 |
| `gain_db` | 浮点数 | `0.0` | 叠加在 `volume` 之上的固定 dB 修正。 |
| `bus` | 字符串 | `"ambience"` | `music`、`ambience`、`sfx`。 |
| `oneshot_stinger` | 布尔值 | `false` | 叠加在所有内容之上播放一次，不停止其他声音。 |
| `pan` | 浮点数 | `0.0` | 声像（pan），-1.0（左）到 1.0（右）。仅在不为 0 时写入。 |

### [[library.scenes]]

| 配置项 | 类型 | 默认值 | 作用 |
|---|---|---|---|
| `id` | 字符串 | 自动生成 | 场景 UUID。 |
| `name` | 字符串 | `""` | 按钮文字。 |
| `color` | 字符串 | 无 | `#rrggbb` 或 `#rgb`。未设置时使用主题配色。 |
| `fade_in_ms` | 整数 | `2000` | 场景声音的淡入时间。控制台允许 0–60000。 |
| `fade_out_ms` | 整数 | `2000` | 场景停止的声音的淡出时间。控制台允许 0–60000。 |
| `curve` | 字符串 | `"equal_power"` | `equal_power` 或 `linear`。 |
| `stop_others` | 布尔值 | `true` | 淡出不在场景中的声音。 |
| `playlist` | 字符串 | 无 | 用作音乐层的播放列表 id。 |

### [[library.scenes.items]]

| 配置项 | 类型 | 默认值 | 作用 |
|---|---|---|---|
| `sound` | 字符串 | 必填 | 声音 id。 |
| `volume` | 浮点数 | `1.0` | 在此场景中的音量，0.0–1.0（替代声音自身的音量）。 |
| `looped` | 布尔值 | `false` | 场景激活期间重复播放。 |
| `start` | 字符串 | `"on_enter"` | `on_enter` 或 `manual`。 |
| `delay_ms` | 整数 | `0` | 激活后的延迟。控制台允许 0–600000。 |
| `pan` | 浮点数 | `0.0` | 非零值替代声音自身的声像。仅在不为 0 时写入。 |

### [[library.scenes.emitters]]

| 配置项 | 类型 | 默认值 | 作用 |
|---|---|---|---|
| `sounds` | 字符串数组 | `[]` | 候选声音 id。重复项会移除。 |
| `min_interval_ms` | 整数 | `20000` | 最短间隔。有效最小值为 100 ms。 |
| `max_interval_ms` | 整数 | `60000` | 最长间隔。若小于最短间隔，则与最短间隔互换。控制台中两者的可选范围均为 100 ms 至 1 小时。 |
| `volume` | 浮点数 | `1.0` | 基础音量，0.0–1.0。 |
| `volume_jitter` | 浮点数 | `0.0` | 音量的随机波动范围，0.0–1.0。 |
| `pan_jitter` | 浮点数 | `0.0` | 立体声声像的随机波动范围，0.0–1.0。 |
| `max_overlap` | 整数 | `2` | 同时播放的实例数。有效最小值为 1。控制台最多提供 16。 |

### [[library.playlists]]

| 配置项 | 类型 | 默认值 | 作用 |
|---|---|---|---|
| `id` | 字符串 | 自动生成 | 播放列表 UUID。 |
| `name` | 字符串 | `""` | 显示名称。 |
| `items` | 字符串数组 | `[]` | 按播放顺序排列的声音 id。 |
| `shuffle` | 布尔值 | `false` | 随机顺序，所有曲目都播放过一遍之前不会重复。 |
| `repeat` | 字符串 | `"all"` | `off`、`all`、`one`。 |
| `crossfade_ms` | 整数 | `2000` | `0` 表示直接切换。控制台允许 0–30000。 |
| `bus` | 字符串 | `"music"` | `music`、`ambience`、`sfx`。 |

列表为空时，不写入场景、播放列表、场景条目和发射器。

<a id="complete-example"></a>
## 完整示例

此文件是有效的 TOML，涵盖所有节。其中的 UUID 仅为示例，请替换它们，或删除 `id` 行，由 Bardwave 自动生成（定时器除外，需要自行指定 `id`）。每个引用都必须与文件中的某个 `id` 匹配。

```toml
# config.toml: annotated example. Omitted keys use the defaults above.

[server]
host = "0.0.0.0"
port = 8080
upnp = true
# access_token = "k3j9x0a7p2m5q8w1"   # generated at start when missing; "" = anyone can listen
# gm_token = "t4n8v2c6b0z3y7u5"       # generated at start when missing; "" = control page on this PC only
tls = false                 # HTTPS and wss://, applied on next Start
# tls_cert = "/etc/ssl/bardwave/fullchain.pem"   # set together with tls_key, or neither
# tls_key = "/etc/ssl/bardwave/privkey.pem"

[audio]
buffer_size = 960           # frames; 0 = driver default
sample_rate = 48000
channels = 2
master_volume = 0.8         # must be 0.0 to 1.0
normalize = false
normalize_target_lufs = -18.0
duck_amount = 0.3
duck_attack_ms = 200
duck_release_ms = 800
duck_buses = ["music", "ambience"]
stream_threshold_secs = 60  # 0 turns streaming off
# output_device = "USB Headphones"
# preview_device = "Built-in Output"

[logging]
level = "info"              # trace | debug | info | warn | error
file_logging = false

[gui]
reduce_motion = false
global_hotkeys = false
# recordings_dir = "/Users/you/Recordings"
# record_float32 = false
check_updates = false
ui_scale = 1.0              # 0.8 to 1.6

[[hotkeys]]
key = "Ctrl+1"
action = { kind = "scene", target = "c3d4e5f6-a7b8-4c9d-8e0f-2a3b4c5d6e7f" }

[[hotkeys]]
key = "F9"
action = { kind = "stop_all" }

[[timers]]
id = "f6a7b8c9-d0e1-4f2a-8b3c-5d6e7f8091a2"
name = "Last call"
trigger = { after_ms = 3600000 }        # once, one hour after Start
action = { kind = "stop_all", fade_ms = 5000 }
enabled = true

[[library.groups]]
id = "a7b8c9d0-e1f2-4a3b-9c4d-6e7f8091a2b3"
name = "Tavern"

[[library.sounds]]
id = "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d"
path = "C:/Bardwave/sounds/tavern-chatter.ogg"
volume = 0.6
looped = true
group = "a7b8c9d0-e1f2-4a3b-9c4d-6e7f8091a2b3"
tags = ["tavern", "crowd"]

[[library.sounds]]
id = "b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e"
path = "C:/Bardwave/sounds/thunder.wav"
bus = "sfx"
oneshot_stinger = true
pan = -0.2

[[library.sounds]]
id = "e5f6a7b8-c9d0-4e1f-8a2b-4c5d6e7f8091"
path = "C:/Bardwave/music/lute.mp3"
bus = "music"

[[library.scenes]]
id = "c3d4e5f6-a7b8-4c9d-8e0f-2a3b4c5d6e7f"
name = "Tavern"
color = "#a04a4a"
fade_in_ms = 2000
fade_out_ms = 2000
curve = "equal_power"
stop_others = true
playlist = "d4e5f6a7-b8c9-4d0e-9f1a-3b4c5d6e7f80"

[[library.scenes.items]]
sound = "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d"
volume = 0.4
looped = true
start = "on_enter"
delay_ms = 0

[[library.scenes.emitters]]
sounds = ["b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e"]
min_interval_ms = 20000
max_interval_ms = 60000
volume = 0.7
volume_jitter = 0.1
pan_jitter = 0.5
max_overlap = 2

[[library.playlists]]
id = "d4e5f6a7-b8c9-4d0e-9f1a-3b4c5d6e7f80"
name = "Tavern music"
items = ["e5f6a7b8-c9d0-4e1f-8a2b-4c5d6e7f8091"]
shuffle = true
repeat = "all"
crossfade_ms = 2000
bus = "music"
```
