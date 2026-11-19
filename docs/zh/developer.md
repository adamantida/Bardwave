# 开发者指南

本指南面向构建、测试或修改 Bardwave 的人员。内容依据仓库中代码的现状编写。关于应用为玩家和主持人（GM）提供的功能，可阅读顶层的 `README.md`。若本指南与代码不一致，以代码为准。

标有 **（未验证）** 的内容未在所述的硬件或操作系统上运行过。

相关页面：[配置参考](./configuration.md)（面向用户的每个 `config.toml` 配置项和命令行选项）、[网络、玩家与主持人远程面板](./network-remote.md)、[HTTPS（安全连接）](./https.md)、[故障排查、常见问题（FAQ）与安全](./troubleshooting-security.md)。

## 目录

- [架构](#architecture)
- [线程模型与音频回调规则](#threading-model-and-audio-callback-rules)
- [类型化命令通道](#typed-command-channels)
- [模块一览](#module-map)
- [传输协议](#wire-protocol)
- [数据模型与配置](#data-model-and-configuration)
- [构建与运行](#build-and-run)
- [测试与布局检查框架](#tests-and-the-layout-harness)
- [打包与发布](#packaging-and-releases)
- [参与贡献与代码风格](#contributing-and-code-style)

<a id="architecture"></a>

## 架构

控制台（egui）与无头模式（headless）服务器（`--server-only`）共用同一个控制核心 `Director`。控制台操作、快捷键、定时器以及手机控制页面最终都会转换为 `Director` 操作，因此场景在各处的行为一致。

```text
egui console ──AudioCommand──┐
hotkeys / timers ────────────┴─▶ Director ──Playback──▶ AudioEngine ──▶ cpal (speakers)
/control: ControlAction ─▶ Board (headless) or GmBridge (console) ─▶ Director
                                                    │ MixedOutput
                                                    ▼
                                  AudioProcessor ─▶ AudioBroadcaster (Opus)
                                                    │
                                       axum HTTP + WebSocket (/ws/audio)
```

- `Director`（`src/audio/director*.rs`）负责场景、发射器（emitter，按随机间隔播放声音的声源）、播放列表、闪避（ducking）和声像（pan）。它没有自己的线程，也不拥有引擎；调用方以 `&impl Playback` 的形式传入引擎。基于时间的工作（延迟的场景启动、发射器、播放列表过渡、交叉淡化（crossfade）的尾段、响度测量结果）在 `Director::tick` 中运行，`Director::next_deadline` 会告知持有者何时唤醒。
- `Playback` 是精简的引擎接口。`AudioEngine` 实现了它，`MockPlayback`（位于 `director.rs` 中）则为测试实现了它。
- `AudioEngine`（`engine.rs`）持有声音目录，打开 cpal 输出并运行试听（preview）。混音器（`mixer.rs`）采用拉取式设计：由设备回调充当时钟。若无法打开任何输出设备，`main.rs` 会回退到无头混音循环，每 10 ms 混音一次，因此听众仍能收到音频流；此时状态栏会显示本机输出已关闭或失败。
- `AudioProcessor`（`main.rs` 中的一个小结构体）从引擎读取 `MixedOutput` 缓冲区，并将其交给广播器。
- 广播器（`network/broadcast.rs`）将混音重新分块为完整的 20 ms 帧，编码为 Opus，再分发给 `/ws/audio` 会话。服务器（`network/server.rs`）是一个 axum 路由器；同一个 `build_router` 提供听众页面、`/api/status` 和 `/ws/audio`，而 `build_router_with_control` 会添加主持人面板的路由。该面板在两种模式下都会挂载，其路由仅在服务器运行期间可访问。

信号路径（来自 `mixer.rs`）：

```text
source -> track gain (fade) -> pan -> bus gain (volume x duck) -> sum -> master -> limiter
       -> [broadcast copy] -> + local-only tracks (headphone previews) -> local limiter -> device
```

仅本机轨道（local-only tracks）在广播副本发布之后才渲染，因此听众永远听不到它们。试听使用固定增益的总线（bus），推子和闪避都不会作用于它；选择了第二台设备时，该设备的回调只渲染试听。

`main.rs` 中的启动顺序：解析 `--profile`、选择数据目录、加载 `config.toml`（解析错误会在此处停止）、设置日志、生成缺失的凭据、在主线程上创建引擎、启动服务器线程、音频处理器线程和听众计数线程，然后运行控制台（`run_gui`）或无头循环。退出时，它会请求服务器线程停止（最多等待 5 s），并停止引擎。从控制台发起的配置方案（profile）切换，会在旧进程释放端口和设备之后再启动。

<a id="threading-model-and-audio-callback-rules"></a>

## 线程模型与音频回调规则

| 线程或任务 | 职责 |
|----------------|--------------|
| 主线程 | `AudioEngine`（其 cpal 音频流为 `!Send`）、`Director` 调用、`tick`；eframe 控制台，或 `--server-only` 的无头循环 |
| cpal 设备回调 | 混音处理；主时钟 |
| 无头混音循环（`mixer.rs`） | 相同的混音，按截止时间控制节奏，仅在没有输出设备时运行 |
| `AudioProcessor` 线程（`main.rs`） | 将混音缓冲区转发给广播器；通道已满时丢弃该帧 |
| `bardwave-stream` | 将长文件解码到 `rtrb` 环形缓冲区 |
| `bardwave-timeline`、`emitters` | 定时器以及随机或周期性声音 |
| 服务器线程 | 独立的单线程 tokio 运行时：axum 服务器、广播器、WebSocket 会话、UPnP 任务；Windows 上的 `netsh` 防火墙规则交给 `spawn_blocking` |
| 短生命周期工作线程 | 文件夹扫描（`library-scan`）、对话框、`decoder` 与 `youtube-download` 任务、`audio-device-scan`、响度测量（`normalize-measure`、`loudness-analyze`）、录音器（`recorder`、`recorder-tee`）、`update-check`、`hotkey-forwarder` |

规则（来自 `Agent.md` 第 2.1 和 4.1 节；属于评审规则，工具不会强制执行）：

- 切勿阻塞渲染线程。文件 I/O、`stat` 调用和对话框都在工作线程上运行，该线程通过通道回复，通道每帧读取一次。
- 在音频回调中只做混音。流式读取路径上不加锁，不分配内存，不做 I/O。发布广播副本时，在有界通道上使用 `try_send`。
- 解码和测量响度的工作都放在回调之外进行。
- 只能在创建引擎的线程上使用引擎。其他线程应通过命令访问它。

> **警告：** 不要从服务器线程调用 `AudioEngine`。应将 `ControlCommand` 放入队列，由主线程执行，主持人面板就是这样做的。

> **注意：** `AppState` 仍持有少量共享的 `Arc<Mutex<…>>` 值（听众数量、UPnP 状态、网络说明、当前生效的 `Config`）。它们是由服务器线程写入的小型状态镜像。不要新增共享的可变状态，应新增命令。

<a id="typed-command-channels"></a>

## 类型化命令通道

| 通道 | 类型 | 容量 | 方向 |
|---------|------|----------|-----------|
| 音频命令 | `AudioCommand`（crossbeam） | 有界 32 | 从 GUI 到引擎和 Director（由 `process_audio_commands` 每帧取出一次） |
| 服务器命令 | `ServerCommand`（crossbeam） | 有界 32 | 从 GUI 到服务器线程；该线程每 50 ms 轮询一次，并将其转发到容量为 32 的 tokio `mpsc` |
| 控制命令 | `ControlCommand`（crossbeam） | 无界 | 从 HTTP 处理程序到主线程 |
| 混音器输出 | `MixedOutput`（crossbeam） | 有界 64（`MixerConfig::channel_capacity`） | 从混音器到 `AudioProcessor`；使用 `try_send`，从不等待 |
| 广播器输入 | `AudioFrame`（tokio `mpsc`） | 500（`engine_channel_capacity`） | 从 `AudioProcessor` 到广播器；使用 `try_send`，通道已满时丢弃该帧 |
| WebSocket 发送队列 | tokio `mpsc` | 100 | 从广播器到单个会话；队列已满时，该客户端的这一帧被丢弃，连接保持不断开 |
| Director 状态 | tokio `watch`，值为 `DirectorState` | 最新值 | 从 Director 到读取方 |
| 正在播放 | tokio `watch`，值为 `NowPlaying`（`NowPlayingFeed`） | 最新值 | 从控制台或无头模式的 `Board` 到广播器 |
| 回复 | `AudioCommand` 和 `ServerCommand` 使用 crossbeam `bounded(1)`；`ControlCommand` 使用 tokio `oneshot` | 1 | 每条命令对应一个应答 |

<a id="module-map"></a>

## 模块一览

路径相对于 `src/`。crate 与二进制文件名为 `soundshared`；产品名称是 Bardwave。

| 路径 | 职责 |
|------|----------------|
| `main.rs` | 入口点、`--server-only`、`--profile`、配置加载、引擎与服务器线程、`AudioProcessor`、无头循环 |
| `lib.rs`、`error.rs` | 库根、`VERSION`；错误类型（`SoundSharedError` 及其组成部分） |
| `config.rs` | `config.toml` 结构、默认值、加载与保存、id 生成、快捷键字符串规范化 |
| `paths.rs` | 数据目录选择与文件路径 |
| `profiles.rs` | 战役配置方案、`--profile`、`recent.toml` |
| `updates.rs` | 默认关闭的更新检查，仅提示通知 |
| `library_scan.rs`、`library_query.rs` | 文件夹扫描与重复检测；搜索 |
| `library_bundle.rs` | 带有 `bundle.toml` 和 `audio/` 的 `.zip` 资源包（bundle） |
| `hotkeys.rs`、`hotkeys/map.rs` | 快捷键绑定解析器；冲突映射 |
| `hotkeys/dispatch.rs`、`hotkeys/local.rs` | 在 Director 上执行动作；控制台聚焦时的按键 |
| `hotkeys/service.rs`、`hotkeys/midi.rs` | 系统级按键（`global-hotkey`）；MIDI，仅在 `midi` feature 下可用 |
| `hotkeys/testing.rs` | 快捷键测试辅助工具 |
| `audio/engine.rs`、`audio/devices.rs` | 声音目录、cpal 输出、试听；设备列表与路由 |
| `audio/decoder.rs`、`audio/stream_source.rs` | Symphonia 解码；流式读取的长文件 |
| `audio/mixer.rs` | 轨道、淡变、声像、总线、限幅器、仅本机轨道 |
| `audio/director.rs` | 场景、状态快照、短音效（stinger）、`Playback`、`MockPlayback` |
| `audio/director_pan.rs`、`audio/director_audio.rs` | 声像；总线、闪避、响度归一化（normalization） |
| `audio/director_timeline.rs`、`audio/timeline.rs` | 定时器动作；场次定时器 |
| `audio/emitter.rs`、`audio/playlist.rs` | 随机与周期性声音；音乐播放列表 |
| `audio/loudness.rs`、`audio/recorder.rs` | EBU R128；WAV 录制 |
| `audio/youtube.rs` | 通过 yt-dlp 和 ffmpeg 导入 YouTube 内容 |
| `network/server.rs` | axum 路由器、`NetworkServer`、静态文件、`/api/status` |
| `network/broadcast.rs`、`network/opus_wire.rs` | 客户端注册表、帧头、正在播放信息流；Opus 打包 |
| `network/websocket.rs` | `/ws/audio` 会话 |
| `network/access.rs` | 访问密钥、主持人令牌守卫、速率限制 |
| `network/control.rs` | 主持人面板：`Board`、`ControlAction`、REST、`/ws/control` |
| `network/tls.rs`、`network/upnp.rs`、`network/local_ip.rs` | HTTPS；路由器映射；对外公布的局域网（LAN）地址 |
| `network/static/` | 听众页面（`index.html`、`app.js`、`style.css`、`opus-decoder.min.js`）、PWA（添加到主屏幕的应用）文件、主持人页面（`control.html/js/css`） |
| `gui/app.rs`、`gui/mod.rs`、`gui/types.rs` | 帧循环、对话框、`sync_director`；共享的 GUI 类型 |
| `gui/groups.rs` | `Library` 模型 |
| `gui/sound_list*.rs`、`gui/library_ui*`、`gui/top_bar.rs` | 声音面板、声音库工具栏与对话框、顶栏 |
| `gui/scenes.rs`、`gui/music*.rs`、`gui/pan_ui.rs` | 场景面板与编辑器；播放列表与发射器（`music_tests.rs` 包含它们的测试）；声像滑块 |
| `gui/hotkeys_ui.rs`、`gui/timeline.rs` | Hotkeys 与 Timers 对话框 |
| `gui/settings_extras*.rs`、`gui/server_panel.rs` | MIX 区块；NETWORK SERVER 面板 |
| `gui/https_ui.rs`、`gui/audio_settings.rs`、`gui/recorder_ui.rs` | HTTPS 区块；AUDIO OUTPUT；REC |
| `gui/profiles_ui.rs`、`gui/onboarding.rs`、`gui/dialog.rs` | 配置方案菜单；首次运行卡片；模态对话框（分组、移动声音、YouTube） |
| `gui/gm_bridge.rs` | 在控制台帧循环中运行主持人命令 |
| `gui/widgets.rs`、`gui/theme.rs`、`gui/motion.rs` | 控件；调色板常量；减少动效 |
| `gui/layout_check.rs`、`gui/layout_probe.rs` | 布局检查器与探针（仅 `#[cfg(test)]`） |

`src/` 之外：`build.rs` 嵌入 Windows 图标（`assets/icon.rc`；未找到 `rc.exe` 时会跳过并给出警告），`examples/` 包含 `audio_probe.rs` 和 `yt_probe.rs`，`packaging/` 包含安装程序的源文件。

<a id="wire-protocol"></a>

## 传输协议

### 听众音频流：`/ws/audio?k=<key>`

缺少或错误的访问密钥会在协议升级前收到 HTTP 401 及 JSON 响应体（`access_token = ""` 表示接受任何密钥）。`/api/status?k=<key>` 的保护方式相同。

每个音频帧是一条二进制消息，带有 8-byte 头部：

| 偏移 | 大小 | 字段 |
|--------|------|-------|
| 0 | 4 bytes | `seq_no`，u32 小端序（little-endian），每帧 +1（64 位计数器截断为 32 位） |
| 4 | 4 bytes | `server_ms`，u32 小端序，自广播器启动以来的 ms 数 |
| 8 | 可变 | 一个 Opus 数据包 |

音频流参数：48 kHz，立体声，每声道 960 个采样（20 ms），Opus，64 kbit/s VBR。无论设备提供何种块大小，都会重新分块为完整帧。暂停超过 120 ms 后，不完整的帧会被丢弃；尾部滞留超过 40 ms 后，会被填充并发送。修改头部大小时，必须同步修改听众页面（`src/network/static/app.js`）和 `tests/websocket_wire_format.rs`。

服务器发送的文本消息：

| `type` | 结构 | 发送时机 |
|--------|------|----------|
| `welcome` | `{"type":"welcome","data":{"client_id":"…","message":"…"}}` | 连接时 |
| `format` | `{"type":"format","data":{"sample_rate":48000,"channels":2,"format":"opus","frame_size":960}}` | 随第一帧发送；若音频流已开始，则紧随 `welcome` 发送；流开始后收到 `get_format` 时发送（开始之前不作应答） |
| `now_playing` | `{"type":"now_playing","scene":"…"\|null,"track":"…"\|null}` | 连接时以及变化时发送，每 200 ms 至多一条；标题仅为名称，最多 120 个字符 |
| `pong` | `{"type":"pong"}` | 对 `ping` 的应答 |

客户端消息：`{"type":"ping"}` 和 `{"type":"get_format"}`。未知类型和二进制消息会被忽略。服务器还会每 30 s 发送一次 WebSocket 层的 ping。访问密钥被吊销时，会话以关闭码 **4001** 关闭；服务器停止时则不附带关闭码。启用 `server.tls` 时，同一路径为 `wss://`。

### 主持人控制：`/ws/control?g=<token>`

帧为不超过 4096 bytes 的 JSON 文本对象。客户端发送 `cmd` 以及可选的 `req`。服务器在连接后以及每次变化后发送 `snapshot`（`rev`、`state`），发送 `ack`（`{"type":"ack","req":…,"ok":true}`）和 `error`（`message`；两者都回显 `req`），以及 `pong`（回显 `req`）。任何未知内容都会以 `error` 回复，且连接保持打开。

客户端命令：`ping`、`state`、`play`、`stop`、`volume`、`loop`、`master`、`stop_all`（可选 `fade_ms`）、`scene`、`stop_scene`、`stinger`、`duck`、`bus`，以及 `playlist_*` 系列（`playlist_start`、`playlist_stop`、`playlist_pause`、`playlist_resume`、`playlist_toggle`、`playlist_next`、`playlist_prev`、`playlist_seek`、`playlist_shuffle`、`playlist_repeat`）。对应的 REST 接口位于 `/api/control/*` 下。限制如下：同时最多 16 个客户端（第 17 个会收到 HTTP 503）；每个客户端每秒最多 100 条命令（超出的命令会收到一条 `error` 并被丢弃）；服务器每 25 s 对空闲客户端发送一次 ping，并断开 75 s 无响应的客户端。主持人令牌被吊销时，连接以关闭码 **4003** 关闭。页面外壳（`/control`、`/control.js`、`/control.css`）对任何人开放；所有读取或修改面板状态的操作都需要主持人令牌或环回地址的对端。访问规则见 `src/network/control.rs` 的模块文档。

<a id="data-model-and-configuration"></a>

## 数据模型与配置

### 声音库模型

`config.toml` 中的 `[library]` 与内存中的 `Library`（`src/gui/groups.rs`）相对应：

- **groups**：`id`、`name`。
- **sounds**：`id`、`path`（必填）、`name`、`volume`、`looped`、`group`、`tags`、`gain_db`、`bus`、`oneshot_stinger`、`pan`。
- **scenes**：`id`、`name`、`color`、`fade_in_ms`、`fade_out_ms`、`curve`、`stop_others`、`items`，以及可选的 `emitters` 和 `playlist`。
- **playlists**：`id`、`name`、`items`、`shuffle`、`repeat`、`crossfade_ms`、`bus`。

场景、播放列表和快捷键通过 **id**（UUID 字符串）引用声音，从不通过路径引用。引擎为已加载声音分配的自身 id（`SoundId`）与此不同，它在 `sync_director` 中建立映射。资源包（`bundle.toml`）包含分组、声音、场景和播放列表，导入时保留 id。

### 配置参考

结构定义位于 `src/config.rs`；默认值即 serde 的默认值。每个节都是可选的。

| 配置项 | 默认值 | 含义 |
|-----|---------|---------|
| `[server] host`、`port` | `"0.0.0.0"`、`8080` | 绑定地址（IP 字面量，而非主机名）及端口 1 到 65535 |
| `[server] upnp` | `true` | 请求路由器映射端口（仅限 IPv4；环回地址绑定时跳过） |
| `[server] access_token` | 首次启动且该配置项缺失时生成 | 访问密钥；`""` 允许任何能连接到该端口的人收听 |
| `[server] gm_token` | 首次启动且该配置项缺失时生成 | 主持人令牌；`""` 使面板仅可从本机访问 |
| `[server] tls`、`tls_cert`、`tls_key` | `false`、未设置、未设置 | HTTPS；自备的 PEM 证书与私钥对，两者都设置或都不设置（只设置其中一个会导致启动失败） |
| `[audio] buffer_size` | `960` | 设备缓冲区（以帧计）；`0` 由驱动程序选择；64 到 8192，其他值回退为 480 并记录警告 |
| `[audio] sample_rate`、`channels` | `48000`、`2` | 请求的设备采样率（8000 到 192000）和声道数（1 到 8）；超出范围的值回退为 48000 和 2。混音和音频流始终保持 48 kHz 立体声 |
| `[audio] master_volume` | `0.8` | 主音量，必须在 0.0 到 1.0 之间；超出此范围的值会使启动因混音器错误而中止 |
| `[audio] normalize`、`normalize_target_lufs` | `false`、`-18.0` | 响度归一化；目标值被限制在 -60 到 0 之间 |
| `[audio] duck_amount` | `0.3` | 闪避时总线降到的音量，限制在 0.0 到 1.0 之间（`1.0` 表示无效果） |
| `[audio] duck_attack_ms`、`duck_release_ms` | `200`、`800` | 闪避时间 |
| `[audio] duck_buses` | `["music", "ambience"]` | 会被闪避的总线 |
| `[audio] stream_threshold_secs` | `60` | 较长的文件以流式方式读取；`0` 表示关闭流式读取 |
| `[audio] output_device`、`preview_device` | 未设置 | 设备名称；未设置时分别表示系统默认设备 / 主输出；找不到的名称回退为默认设备 |
| `[logging] level`、`file_logging` | `"info"`、`false` | `RUST_LOG` 会覆盖级别；`file_logging` 还会在根数据目录中写入 `bardwave.log` |
| `[gui] reduce_motion`、`global_hotkeys` | `false`、`false` | 动画；系统级快捷键 |
| `[gui] recordings_dir`、`record_float32` | 未设置、`false` | WAV 录音文件夹（默认为数据目录中的 `recordings/`）；32 位浮点 |
| `[gui] check_updates`、`last_update_check` | `false`、`0` | 每日更新检查；上次检查的 Unix 时间 |
| `[gui] ui_scale` | `1.0` | 限制在 0.8 到 1.6 之间（非有限值取 1.0） |
| `[gui] onboarding_done` | `false` | 首次运行卡片已完成 |

各条目的默认值：声音的 `volume` 为 `1.0`，`looped` 为 `false`，`bus` 为 `"ambience"`，`pan` 为 `0.0`，`gain_db` 为 `0.0`；场景的 `fade_in_ms` 与 `fade_out_ms` 为 `2000`，`curve` 为 `"equal_power"`（或 `"linear"`），`stop_others` 为 `true`；场景条目的 `volume` 为 `1.0`，`start` 为 `"on_enter"`（或 `"manual"`），`delay_ms` 为 `0`；发射器的 `min_interval_ms` 为 `20000`，`max_interval_ms` 为 `60000`，`volume` 为 `1.0`，`max_overlap` 为 `2`，`volume_jitter` 与 `pan_jitter` 为 `0.0`；播放列表的 `repeat` 为 `"all"`（`"off"`、`"one"`），`crossfade_ms` 为 `2000`，`bus` 为 `"music"`；定时器的 `trigger` 为 `{ after_ms = 600000 }`，`enabled` 为 `true`。总线为 `music`、`ambience`、`sfx`。

快捷键与定时器的动作以 `kind` 作为标记。快捷键类型：`sound`、`scene`、`playlist`（均带有 `target`）、`stop_all`、`stop_scene`、`master_up`、`master_down`、`master_mute`、`duck_toggle`、`duck_hold`、`next_track`、`prev_track`、`pause_music`。定时器类型：`activate_scene`（`scene`）、`stop_scene`、`play_stinger`（`sound`）、`stop_all`（`fade_ms`）、`start_playlist`（`playlist`）、`stop_playlist`、`next_track`、`pause_music`、`resume_music`。定时器的触发条件为 `{ after_ms = … }` 或 `{ every_ms = … }`；最短间隔为 100 ms。快捷键的 `key` 由修饰键和一个按键组成，二者以 `+` 连接；`canonical_hotkey` 会将其规范化（`"shift+ctrl+a"` 会变为 `"Ctrl+Shift+A"`）。

### 示例

```toml
[server]
port = 8080
upnp = true
tls = false

[audio]
master_volume = 0.8
duck_buses = ["music", "ambience"]

[logging]
level = "info"

[[library.groups]]
id = "7f0c1e52-0a4b-4c1e-9f1e-3a2b5c6d7e01"
name = "Tavern"

[[library.sounds]]
id = "7f0c1e52-0a4b-4c1e-9f1e-3a2b5c6d7e02"
path = "/home/gm/sounds/tavern-crowd.ogg"
name = "Crowd"
looped = true
group = "7f0c1e52-0a4b-4c1e-9f1e-3a2b5c6d7e01"
tags = ["crowd"]
bus = "ambience"

[[library.scenes]]
id = "7f0c1e52-0a4b-4c1e-9f1e-3a2b5c6d7e03"
name = "Tavern evening"
fade_in_ms = 2000
fade_out_ms = 2000
curve = "equal_power"
items = [
  { sound = "7f0c1e52-0a4b-4c1e-9f1e-3a2b5c6d7e02", volume = 0.6, looped = true },
]

[[hotkeys]]
key = "Ctrl+1"
action = { kind = "scene", target = "7f0c1e52-0a4b-4c1e-9f1e-3a2b5c6d7e03" }

[[timers]]
name = "Last call"
trigger = { after_ms = 600000 }
action = { kind = "stop_all", fade_ms = 3000 }
enabled = true
```

### 兼容性规则

- 每个新字段都有 serde 默认值，因此旧文件无需修改即可加载。结构变更需要在 `tests/config_compat.rs` 中增加一个用例（它确保 v1.4.0 的文件仍可加载）。
- `id` 缺失、为空、不是 UUID 或重复的声音、场景或播放列表，会在加载时获得新的 UUID（`ensure_ids`），并在下次保存时写入。声音库内部的引用以及快捷键，会跟随被替换的手写 id。定时器不会被重新映射。
- 缺少 `config.toml` 时使用默认值。无法解析的 `config.toml` 会使应用以错误停止（控制台中显示对话框，使用 `--server-only` 时输出到 stderr）。它绝不会被默认值替换，否则下一次保存会覆盖声音库。（`Config::load` 上的文档注释仍写着会回退到默认值；但 `main.rs` 并不会这样做。）
- 保存时会根据内存中的模型整体重写文件（`toml::to_string_pretty`），因此手动编辑文件中的注释和键顺序不会被保留。
- 定时器仅以定义的形式保存，且始终以停止状态启动。
- `[[hotkeys]]` 和 `[[timers]]` 属于控制台设置。它们不包含在资源包中，也不会在 `--server-only` 模式下运行。

### 数据目录

`src/paths.rs` 通过纯函数 `choose_data_dir` 决定目录：

| 情形 | 目录 |
|-----------|-----------|
| `portable.flag` 位于可执行文件旁 | 程序所在文件夹 |
| `config.toml` 位于可执行文件旁（在 Windows 上仅当该文件夹可写时） | 程序所在文件夹 |
| 可执行文件所在文件夹可写，且不在 macOS `.app` 内 | 程序所在文件夹 |
| 其他情况，macOS | `~/Library/Application Support/Bardwave` |
| 其他情况，Windows | `%APPDATA%\Bardwave` |
| 其他情况，Linux 及其他 Unix | `$XDG_DATA_HOME/bardwave` 或 `~/.local/share/bardwave` |

这就是根目录。若无法推导出系统位置（既无 `HOME` 也无 `APPDATA`），则使用程序所在文件夹。`--profile <name>`（或 `--profile=<name>`）会把 `config.toml` 和 `bardwave-library/` 移至根目录下的 `profiles/<name>/`；日志、`tools/` 和 `recent.toml` 保留在根目录中，并由各配置方案共享。`--profile default` 表示根配置方案。名称长度为 1 到 32 个字符，可包含字母、数字、空格、`-` 和 `_`；`default` 以及 `CON` 等 Windows 设备名称会被拒绝。名称无效或无法创建配置方案文件夹时，会显示错误并以退出码 2 退出。

> **注意：** 各操作系统的选择逻辑在任何主机上都有单元测试，但并未在每个操作系统上检查 **（未验证）**。

<a id="build-and-run"></a>

## 构建与运行

系统要求以 `Agent.md` 和 `README.md` 中的说明为准：Rust 1.85 或更新版本（`Cargo.toml` 中没有 `rust-version`，因此 Cargo 不会强制检查；未验证）。Opus 为纯 Rust 实现（`rusty-opus`），因此无需 CMake 或 C 编译器。在 Debian 或 Ubuntu 上，CI 会安装 `pkg-config libgtk-3-dev libasound2-dev libxkbcommon-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libssl-dev`。在 macOS 和 Linux 上，YouTube 导入需要 `ffmpeg`；在 Windows 上，首次使用时会下载它。

```bash
cargo build --release --locked
cargo run --release                              # console
cargo run --release -- --server-only             # headless, serves /control
cargo run --release -- --profile "Camp 1"        # campaign profile
cargo build --release --locked --features midi   # optional MIDI module
```

命令行选项仅有 `--server-only` 和 `--profile`；其他参数会被忽略。可执行文件为 `target/release/soundshared`（`soundshared.exe`）。Windows 发行版构建使用 GUI 子系统：从终端启动时，它会重新附加到该终端；从资源管理器启动 `--server-only` 时，会打开它自己的控制台窗口。在 Windows 上，`build.bat` 封装了常用命令（`release`、`test`、`clean`、`fmt`、`clippy`）。

<a id="tests-and-the-layout-harness"></a>

## 测试与布局检查框架

```bash
cargo test --all-targets --locked
cargo clippy --all-targets --locked
cargo clippy --all-targets --locked --features midi
cargo fmt --all --check
```

`src/` 和 `tests/` 中约有 1,190 个测试属性（1,131 个 `#[test]` 和 60 个 `#[tokio::test]`；这是文本计数，并非实际运行结果）；`Agent.md` 和 `README.md` 仍写着“~780”。`tests/` 中有 33 个文件，其中包括 `config_compat.rs`、`websocket_wire_format.rs` 和 `gm_control_auth.rs`。`docs_gm_token.rs` 会读取 `README.md`、`packaging/README.txt` 和 `packaging/config.toml`，因此修改时须保证 `gm_token` 与 `?g=` 的表述正确。

控制台布局在无头模式下进行测试。`src/gui/layout_check.rs` 通过 `egui::Context::run` 运行一个完整的控制台帧（`GuiApp::draw_frame`），读取已绘制的图形，并报告：

- `ClipOverflow`：文本被其裁剪区域裁切超过 1 px；
- `Overlap`：文本在水平和垂直两个方向上均重叠超过 2 px；
- `OffScreen`：绘制内容超出窗口；
- `IdClash`：egui 的“两个控件共享同一 id”警告；
- `Hidden`：必需控件被截断、滚动移出或未绘制。必需控件包括“Stop all”（全部停止）、“Mute”（静音）和“Add Sound”（添加声音），服务器运行时还包括“Stop Server”（停止服务器）和“Copy Link”（复制链接），服务器停止时则包括“Start Server”（启动服务器）。

滚动区域边缘的垂直裁切不视为违规（半可见的行正是滚动的正常表现）；水平方向的裁切则从不予以豁免。测试 `the_console_has_no_layout_violations_in_any_size_scale_and_state` 会在五种窗口尺寸（从 900×600 到 1600×900）和两种界面缩放（1.0 与 1.3）下，针对五种状态（声音库为空或名称很长，场景数为 0 或 2，服务器停止或运行）运行检查，因此回归会导致 `cargo test` 失败。`layout_probe.rs` 提供文本框辅助函数，顶栏、音乐和场景面板的测试使用它们检查各自的宽度。该检查框架无法检测以下情况：文本溢出到相邻面板、提示、已打开的对话框。它不会访问音频设备、网络或用户的配置。

<a id="packaging-and-releases"></a>

## 打包与发布

| 工作流 | 触发条件 | 输出 |
|----------|---------|--------|
| `ci.yml` | 向 `main` 的推送以及以其为目标的拉取请求（pull request）；手动 | 在 Ubuntu、Windows、macOS 上运行 fmt、clippy、测试以及 MIDI clippy；警告视为错误（`RUSTFLAGS` 来自 `setup-rust-toolchain`） |
| `release.yml` | 推送到 `main` 且 git 标签 `v<Cargo.toml version>` 为新标签时；或手动运行并指定匹配的版本 | 测试；Windows x86_64 zip 与 Linux x86_64 tar.gz（ubuntu-22.04）；标签与发行版；检查 `Cargo.lock` 的版本是否一致 |
| `release-extras.yml` | 发行版发布时，或 `Release` 运行成功后；或手动并指定标签 | Inno Setup 安装程序（`packaging/windows/bardwave.iss`）和单独的 `.exe`；AppImage 与 `.deb`（cargo-deb）；冒烟测试；`SHA256SUMS` |
| `release-macos.yml` | 发行版发布时，或手动并指定标签 | 通用版（aarch64 与 x86_64）的 `Bardwave.app`，装在 `.dmg` 中，另附其 `.sha256`；更新 `SHA256SUMS` |

`release.yml` 使用内置令牌创建的发行版不会触发 `release: published` 事件，因此 `release-macos.yml` 不会针对它自动启动，需要手动运行并指定标签。`release-extras.yml` 为这种情况设有自己的 `workflow_run` 触发器。

`packaging/` 中的源文件：`README.txt`（纯文本快速入门）和 `config.toml`（带注释的示例）会被打包进 zip 和 tar.gz；`windows/bardwave.iss`；`linux/`（`build-appimage.sh`、`bardwave.desktop`）；`macos/`（`build-app.sh`、`make-dmg.sh`、`sign-notarize.sh`、`entitlements.plist`、`README.md`）。`.deb` 由 `Cargo.toml` 中的 `[package.metadata.deb]` 描述（软件包 `bardwave`，二进制文件安装为 `/usr/bin/bardwave`）。

> **警告：** 请勿在无关的工作中修改 `Cargo.toml` 中的 `version`。推送到 `main` 会为该版本发布一个发行版，而版本号由所有者决定。`Cargo.lock` 必须包含相同的版本（修改版本号后运行 `cargo build` 并一并提交两者）。

> **注意：** 仅当设置了 Apple 机密时才会运行 macOS 签名和公证；未设置时，会附加 ad-hoc 签名的构建。安装程序、macOS 应用和发行工作流尚未在真实机器上运行 **（未验证）**；参见 `CHANGELOG.md`。

<a id="contributing-and-code-style"></a>

## 参与贡献与代码风格

请保持文档真实准确。功能变更需同时更新 `README.md`、`Agent.md` 和 `CHANGELOG.md`。提交较大的拉取请求之前，先创建一个 issue。

- 使用类型化命令。不要在 GUI、引擎和网络之间共享可变状态。
- 切勿阻塞渲染线程，音频回调中只做混音。
- 通过 id 引用声音。
- 新的配置字段需添加 `#[serde(default)]`，结构变更需要在 `tests/config_compat.rs` 中添加测试。
- 使用 `anyhow::Result` 和 `thiserror`。panic 通过 `main.rs` 中的钩子处理。使用 `tracing` 记录日志（`RUST_LOG` 会覆盖 `[logging] level`）；切勿记录 `?k=`、`?g=` 或令牌值。
- 运行 `cargo fmt --all`。保持 clippy 零警告，并确保 `midi` feature 可以正常编译。

拉取请求的目标分支为 `main`。处于打开状态的拉取请求会运行 CI，但不会创建发行版。

### 现象与原因

| 现象 | 原因 | 查看位置 |
|---------|-------|---------------|
| 启动时出现错误对话框，应用无法打开 | `config.toml` 无法解析 | 消息中会指出该文件；修复它，应用绝不会将其重置 |
| 启动时出现错误对话框（或 stderr）并以退出码 2 退出 | `--profile` 名称无效，或无法创建配置方案文件夹 | `src/profiles.rs`、`src/paths.rs` |
| 本机没有声音，但玩家能听到音频流 | 无法打开本机输出设备；引擎以无头模式运行 | 状态栏显示 `Local off — headless (network only)` 或 `Local failed — <error>`；`main.rs`、`top_bar.rs` |
| 听众收到关闭码 4001 | 访问密钥已重新生成 | `network/access.rs`；将新链接发给玩家 |
| 主持人页面返回 401，随后返回 429 | `?g=` 令牌错误；一分钟内失败 10 次后 | `network/access.rs`（`GM_MAX_FAILURES`）；等待一分钟 |
| `/ws/control` 以 4003 关闭 | 主持人令牌已被吊销 | 使用当前的 `?g=` 链接重新加载页面 |
| 服务器无法启动，状态显示 Error，按钮变为 Retry Server（重试服务器） | 绑定失败，例如端口被占用，或 HTTPS 设置失败 | `main.rs` 中的 `start_listener` 通过命令的回复通道将错误返回给 GUI |
| HTTPS 启动失败 | 证书或私钥无法读取或不匹配，或两者只设置了一个 | [HTTPS（安全连接）](./https.md)、故障排查表 |

### 已知限制

- 快捷键与定时器不会在 `--server-only` 模式下运行。
- HTTPS 默认关闭；在启用之前，链接中的密钥以明文 HTTP 传输。参见 [HTTPS（安全连接）](./https.md)。
- 响度归一化会跳过流式读取的文件。
- MIDI 位于可选的 `midi` feature 之后（默认关闭），且未连接到控制台；MIDI 绑定会被报告为不可用。
- Linux 上的全局快捷键仅在 X11 下工作，在 macOS 上 **（未验证）**。Windows 和 Linux 上的音频设备切换 **（未验证）**。
- 没有浅色主题（调色板为 `gui/theme.rs` 中的常量），也没有面向玩家的分总线音量（所有玩家共享一条音频流）。
