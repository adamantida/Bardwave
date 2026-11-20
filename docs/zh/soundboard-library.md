# 声音库

声音库是控制台中的 `01 / Library` 面板（标题为 **Sound Library**（声音库），并带有 `12 Tracks` 这样的计数）。它保存控制台能够播放的全部声音、这些声音所属的分组和标签，以及每个声音携带的设置，如音量、重复播放与声像（pan）。本页说明如何添加声音、整理声音、修复缺失的文件、将声音作为资源包（bundle）共享，以及调整相关的音频设置。下文中的文字和取值均取自应用程序代码。部分文字在屏幕上以大写字母显示（例如 `FILE NOT FOUND`、`NO MATCHES`）；本页采用代码中的写法。未在真实硬件上检查过的行为标记为 **（未验证）**。

## 目录

- [声音的添加](#adding-sounds)
- [支持的格式](#supported-formats)
- [单个声音的设置](#per-sound-settings)
- [分组](#groups)
- [标签与搜索](#tags-and-search)
- [缺失文件](#missing-files)
- [资源包](#bundles)
- [耳机试听](#headphone-preview)
- [响度归一化](#loudness-normalization)
- [长文件流式读取](#streaming-long-files)
- [配置参考](#configuration-reference)
- [故障排查](#troubleshooting)

声音库保存在 `config.toml` 的 `[library]` 部分中。添加的声音记录在该处，而不是音频文件本身：声音库只保存每个文件的路径。用户自己的音频文件永远不会被声音库修改或删除；移除声音只会删除其条目。

<a id="adding-sounds"></a>
## 声音的添加

面板标题栏有两个按钮：**Add Sound**（添加声音）和 **YouTube**。**Folder…**（扫描文件夹）、**Export…**（导出）和 **Import…**（导入）位于其下方的搜索行中。

在 **Groups**（分组）栏中选中某个分组后添加的声音会进入该分组（**All Sounds**（全部声音）和 **Ungrouped**（未分组）除外）。这一规则同时适用于 **Add Sound** 和 YouTube 下载。文件夹导入会自动创建分组，见下文。

新声音的名称取文件名（不含扩展名）；YouTube 下载的声音则取视频标题。新声音的音量为 100%。已在声音库中的文件（路径相同）不会被重复添加。

### Add Sound

点击面板标题栏中带加号图标的 **Add Sound** 按钮。空列表中也会显示同一按钮。系统文件选择器会以标题 `Select Audio Files` 打开，过滤器 `Audio Files` 设为 `mp3, wav, ogg, flac, opus`。可以一次选择多个文件。每个文件都会发送给音频引擎，解码完成后出现在列表中。

> **注意：** 无法解码的文件不会被添加。原因会写入日志（`Failed to load sound`）。界面不会为此显示任何消息。

### Folder…

点击搜索行中的 **Folder…** 按钮。其提示文字为 `Add every sound in a folder (and its sub-folders)`。

1. 在标题为 `Choose a folder with sounds` 的文件选择器中选择一个文件夹。
2. **Add Folder**（添加文件夹）对话框会递归扫描该文件夹（最多深入 64 层）。条目按排序顺序访问，先处理文件夹自身的文件，再处理子文件夹。扫描只读取，不会改变声音库。
3. 对话框先显示实时进度，然后显示新声音的数量（`N new sounds found`）；如果存在子文件夹，还会显示 **Sub-folders**（子文件夹）列表，列出每个子文件夹中的声音数量。
4. 如果希望每个一级子文件夹对应一个分组，勾选 **Create a group for each sub-folder**（为每个子文件夹创建分组）；发现子文件夹时，该复选框默认已勾选。更深层级的文件会进入其所属一级子文件夹的分组。对话框会列出将要创建的分组（最多 4 个名称），以及将复用的现有分组数量。名称相同（不区分大小写）的分组会被复用。分组名称会被截断为 32 个字符。直接位于所选文件夹中的文件保持未分组。
5. 点击 **Add N Sounds**（添加 N 个声音）以添加它们（未发现新声音时该按钮不可用）。点击 **Cancel**（取消）将关闭对话框，不添加任何内容。

扫描期间，点击 **Cancel** 会关闭对话框并停止扫描，不会添加任何内容。

添加声音后，搜索行下方会显示几秒钟的消息，例如 `Added 12 sounds, 2 new groups`。

扫描器会跳过以下内容：

- 已在声音库中的文件（按路径判断）；
- 与另一文件或声音库中已有文件内容相同的文件（大小相同，且首尾各 64 KiB 的哈希值相同；不超过 128 KiB 的文件会对全部内容计算哈希）；
- 空文件和 AppleDouble 附属文件（`._name.mp3`）；
- 指向被扫描文件夹之外的符号链接，以及损坏的链接。通过链接访问到的文件会最后处理，因此真实文件优先于指向它的链接。

对话框会分别说明每种原因跳过的数量，例如 `3 duplicates skipped (same content as another file)`，并说明有多少条目无法读取。

### YouTube

点击面板标题栏或空列表中的 **YouTube** 按钮。面板较窄时，标题栏中的该按钮会最先隐藏。对话框为 **Add From YouTube**（从 YouTube 添加），其中有 `Video or music link` 输入框。点击 **Add**（添加）或按 Enter。

支持的链接：

- `https://www.youtube.com/watch?v=<id>`（额外参数如 `&t=` 或 `&list=` 会被忽略：带有 `&list=` 的链接只添加该视频）；
- `https://youtu.be/<id>`；
- `https://music.youtube.com/watch?v=<id>`（任何 `youtube.com` 子域名以及 `youtube-nocookie.com` 均可）；
- `/shorts/<id>`、`/embed/<id>` 和 `/live/<id>` 地址；
- 仅含 11 个字符的视频 ID。

播放列表页面、频道页面及其他网站会被拒绝，并显示 `Not a YouTube video link`。

音频通过 yt-dlp 下载一次。文件保存在当前配置方案（profile）的 Bardwave 数据目录下的 `bardwave-library` 文件夹中，因此重新启动后仍然保留。下载内容由 ffmpeg 转换为 MP3（160K）。文件名为视频标题（截断为 80 个字符，并替换掉 Windows 中不允许使用的字符），后接括号中的视频 ID。列表行只显示标题，不显示 ID。已下载过的视频会直接复用（`Already downloaded — reusing`），不产生任何网络流量。

首次使用需要辅助工具。对话框会显示 `First use downloads yt-dlp + ffmpeg once (~230 MB), then it just works`。进度行中给出 yt-dlp 约 24 MB、ffmpeg 约 100 MB。下载过程中，对话框会显示以下进度信息（`Connecting…`、`Downloading yt-dlp…`、`Fetching video info…`、`Downloading audio…`）。

Bardwave 按以下顺序查找每个工具：先在可执行文件旁，再在 `PATH` 中，最后在其自身的 `tools` 文件夹中（位于根数据目录中，由所有配置方案共享）。用户自行安装的副本会优先于下载的副本使用。

| 工具 | 何时下载 | 缺失时的处理 |
|---|---|---|
| yt-dlp | 首次使用时下载，约 24 MB，来自其 GitHub 发行版 | 在 Windows、macOS 和 Linux（x86-64 与 ARM64）上自动下载。在 macOS 和 Linux 上未验证。其他系统会提示手动安装 |
| ffmpeg | 首次使用时下载，约 100 MB（Windows 版本） | 仅在 Windows 上自动下载。在其他系统上，下载会以 `ffmpeg not found on this system.` 中止。需自行安装，例如使用 `brew install ffmpeg` 或 `apt install ffmpeg` |

yt-dlp 的错误（视频已被删除、地区限制、无网络）会显示在对话框中。

> **注意：** YouTube 一次只能下载一个。下载期间会显示 **Close**（关闭）以代替 **Cancel**；**Close** 和 Esc 都不会取消下载：对话框会关闭，下载在后台继续，完成后声音会出现。下载期间，**YouTube** 按钮不起作用。如果在关闭对话框后下载失败，错误只会写入日志。

<a id="supported-formats"></a>
## 支持的格式

文件使用 Symphonia 解码。构建时启用了 `vorbis`、`mp3`、`flac`、`wav` 和 `aac` 这几个 Cargo feature（`Cargo.toml`）。解码后的音频会转换为 48 kHz 立体声（单声道会复制到两个声道，多声道文件会下混为两个声道）。

| 扩展名 | 状态 | 说明 |
|---|---|---|
| `.wav` | 支持 | Add Sound、文件夹扫描、Locate（重新定位）、资源包 |
| `.mp3` | 支持 | Add Sound、文件夹扫描、Locate、资源包 |
| `.ogg` | 支持 | Vorbis。Add Sound、文件夹扫描、Locate、资源包 |
| `.flac` | 支持 | Add Sound、文件夹扫描、Locate、资源包 |
| `.aac` | 已列出 | 文件夹扫描、Locate 和资源包都接受该格式，但 Add Sound 过滤器中没有提供。播放未验证 |
| `.m4a` | 已列出 | 文件夹扫描、Locate 和资源包都接受该格式，但 Add Sound 过滤器中没有提供。构建中未启用 MP4 容器，因此播放未验证 |
| `.opus` | 未验证 | Add Sound 过滤器中有该格式，但文件夹扫描、Locate 和资源包都不接受，并且没有启用 Opus 解码器。应视为不受支持 |

文件夹扫描、**Locate…** 和资源包导入使用相同的扩展名检查（不区分大小写），仅 `.opus` 例外，它只出现在 Add Sound 过滤器中。如果 **Locate…** 收到扩展名不同的文件，会显示消息 `That file is not a supported audio file`。

<a id="per-sound-settings"></a>
## 单个声音的设置

每个声音行从左到右有以下控件。该行还显示名称、类似 `48 kHz / 00:42 / WAV` 的信息行（在较窄的行中会最先隐藏）以及标签。

| 控件 | 用法 | 范围 | 默认值 | 存储为 |
|---|---|---|---|---|
| 播放 / 停止按钮 | 点击左侧的圆形按钮 | — | — | — |
| Repeat（重复） | 点击方形图标。提示为 `Repeat: on` 或 `Repeat: off`。立即生效，正在播放的声音也会生效 | on / off | off | `looped` |
| Preview（试听） | 耳机图标。参见 [耳机试听](#headphone-preview) | — | — | 不存储 |
| Volume（音量） | 点击或拖动该行的滑块。旁边会显示百分比 | 0% 到 100% | 100% | `volume`（0.0 到 1.0） |
| Group（分组） | 点击该行的分组名称。提示为 `Change group` | — | 无分组 | `group` |
| Remove（移除） | 点击 ✕ 图标 | — | — | 移除该声音 |

在非常窄的行中，滑块和分组名称会隐藏。将鼠标悬停在某行上可查看其完整名称、路径和分组。各行按声音加载的顺序列出；文件缺失且未加载的声音排在最后。

行上的 **Remove**（✕）会立即从声音库中移除该声音，不会弹出确认。它还会将该声音从所有使用它的场景、发射器（emitter，按随机间隔播放声音的声源）、播放列表和快捷键中移除。磁盘上的音频文件不会被改动。

### Pan

声像可以通过行的右键菜单中的 **Pan** 区块设置。

- 滑块范围从 `L 100%` 到 `R 100%`。为零时显示 `Centre`。非常接近中心的值（小于 5%）会自动吸附到中心。
- 双击滑块，或在其获得焦点时按 Home，可将其重置为中心。
- 使用键盘时，方向键以 5% 为步长移动滑块。按住 Shift 则以 25% 为步长。
- 移动滑块会立即改变声音，播放期间也同样生效。
- 居中的声像不会写入配置文件。

### 短音效与场景

同一右键菜单中有 **Use As Stinger**（用作短音效）或 **Remove Stinger Flag**（移除短音效标记），以及 **Add to scene**（添加到场景）。短音效（stinger）是叠加在场景之上的单次播放声音。它不会停止其他声音，并会在场景面板中获得一个触发键。场景不在本页的讨论范围内。

> **注意：** 声音的混音总线（`bus`）是一项配置设置，取值为 `music`、`ambience` 或 `sfx`。未在声音行或其菜单中找到该设置的控件（未验证）。

<a id="groups"></a>
## 分组

分组是声音库的分类。它们列在面板左侧的 **Groups** 栏中。

- **All Sounds** 显示所有声音及其总数。
- **Ungrouped** 显示未分组的声音。
- 分组排列在 **Categories**（分类）文字之下。每个分组都会显示其声音数量，数量随当前的搜索和标签筛选而变化。
- 点击分组栏标题中的 **+** 图标（提示为 `New group`）即可创建分组。新分组会成为选中项。
- 将鼠标悬停在分组上，会在其数量的位置显示编辑图标（提示为 `Rename group`）和 ✕ 图标（提示为 `Delete group`）。

分组名称会去除首尾空格，最多 32 个字符，且必须唯一（不区分大小写）。对话框会显示 `Up to 32 characters`。被拒绝的名称会显示 `Enter a group name`、`Maximum 32 characters` 或 `A group with this name already exists`。

| 操作 | 对话框 | 按钮 | 效果 |
|---|---|---|---|
| 创建 | **New Group**（新建分组） | **Create**（创建） | 添加一个空分组 |
| 重命名 | **Rename Group**（重命名分组） | **Save**（保存） | 声音保持原位 |
| 删除 | **Delete Group**（删除分组） | **Delete**（删除），或 **Keep**（保留）以放弃 | 声音不会被删除。它们会留在声音库中并变为未分组。副标题 `This cannot be undone` 指的是分组 |

要将声音移到另一分组，点击其所在行的分组名称。**Move Sound**（移动声音）对话框（`Assign to`）会列出 `Ungrouped` 和所有分组。点击其中一项即可分配该声音。如果没有任何分组，对话框会显示 `No groups yet`。

窗口较窄时，分组栏会变窄，并可能隐藏。各行仍可正常使用。

<a id="tags-and-search"></a>
## 标签与搜索

搜索框的占位文字为 `Search name or tag…`。文字旁会出现一个图标按钮（提示为 `Clear search`）。

- 搜索词只要是声音名称、文件名或任一标签的一部分文字，即视为匹配。
- 所有 Unicode 字母的匹配均不区分大小写。重音符号不会被忽略：`echo` 无法找到 `écho`。
- 多个词必须全部匹配（AND）。
- 搜索在所选分组内进行。

标签以标签块的形式显示在搜索行下方。每个标签块显示在当前筛选条件下具有该标签的声音数量。点击标签块即可将其加入筛选；再次点击则取消。声音必须同时具有所有选中的标签。已选中的标签块排在最前，之后是使用最多的标签。最多显示 14 个标签块，之后是 `+N more tags`。

启用筛选时，会显示一行 `Showing N of M sounds`（M 为整个声音库的数量）。**Clear**（清除）会移除搜索内容和标签选择。如果没有任何匹配项，面板会显示 `NO MATCHES` 和 **Clear Filters**（清除筛选）。

### 标签的编辑

点击行中的标签文字（提示为 `Edit tags`；该行最多显示四个标签，格式为 `#tag`，之后显示 `+N`）。在没有标签的行上，将鼠标悬停即可显示 `+ TAG`。标记为 `File not found` 的行则会在该位置改为显示 `LOCATE…`。**Edit Tags**（编辑标签）对话框包含：

- `Tags on this sound`，每个标签旁有一个 `Remove this tag` 按钮；
- `Add tags (separate with commas)`，带有 **Add** 按钮（也可以按 Enter）；
- `Already used`，会推荐最多 8 个与输入内容匹配、且该声音尚未具有的现有标签；
- **Done**（完成）。

标签会去除首尾空格，以小写形式保存，最多 32 个字符，并且不会重复。超过 32 个字符的标签会被拒绝，并显示 `"…" is too long (at most 32 characters)`，该次输入的内容一个都不会添加。

<a id="missing-files"></a>
## 缺失文件

文件已丢失的声音会保留在声音库中。其行会显示琥珀色徽标 `File not found`、琥珀色边框以及 `LOCATE…` 链接（提示为 `Pick the file's new location`）。完全无法加载的声音会显示较短的行，带有相同的徽标、路径和 **Locate…** 按钮。该声音在场景、播放列表和快捷键中的所有引用都会保留。

列表上方会显示一条琥珀色横幅 `N files not found`，并带有两个按钮：

- **Replace Path Prefix…**（替换路径前缀，整个文件夹被移动时使用）；
- **Recheck**（重新检查，提示为 `Look for the files again`）。

声音库还会在后台约每 20 秒重新检查一次文件是否存在。只有在保存的路径上确实存在该文件时，才视为文件存在。

> **注意：** 该检查只更新未找到的标记。启动时因驱动器未挂载而无法加载的声音，会在 **Locate…**、**Replace Path Prefix…** 或导入资源包之后重新加载，或在重新启动 Bardwave 时加载。挂载驱动器之后，仅靠 **Recheck** 能否恢复此类声音（未验证）；代码只在上述三种操作之后请求重新加载。

### Locate

点击行上的 **Locate…** 或 `LOCATE…` 链接。文件选择器标题为 `Locate the sound file`，初始位置为旧路径上方仍然存在的最近文件夹。所选文件会替换旧路径（`Relinked "name"`）。声音保留其 ID，因此场景和快捷键仍然有效。如果新文件已被另一个声音使用，声音库会拒绝并显示 `That file is already used by another sound in the library`。

### Remove

缺失文件行上的 ✕ 图标（提示为 `Remove from the library`）会移除该声音。它还会将该声音从场景、发射器、播放列表和快捷键中移除，并告知已移除的引用数量。

### Replace Path Prefix

当整个声音文件夹被移动时使用此功能，例如从 `/Volumes/Old/Sounds` 移到 `/Volumes/New/Sounds`。

1. 点击 **Replace Path Prefix…**。旧路径开头会预先填入缺失文件所共有的文件夹。
2. 在 **Old start of the path**（旧路径开头）中检查或输入路径的旧开头部分。
3. 在 **New start of the path**（新路径开头）中输入新的开头部分。点击该输入框旁的 **Browse…**（浏览）可选择新文件夹。
4. 对话框会显示将更改的路径数量（`N paths will change`）、其中当前未找到的数量，以及最多三个示例。如果没有任何匹配项，会显示 `No sound path starts with that prefix.`，并且 **Replace**（替换）按钮不可用。
5. 点击 **Replace**。随后会显示 `Rewrote N paths` 消息，并重新检查文件。

匹配按完整的路径组成部分进行，因此 `/music` 不会匹配 `/music2/a.wav`。`/` 和 `\` 都可作为分隔符，当两个前缀使用不同风格时，分隔符会被转换。即使文件在新位置不存在，路径也会被改写。会使两个声音共用同一路径的改写会被略过。

<a id="bundles"></a>
## 资源包

资源包是一个 `.zip` 文件，用于将一组声音传递到另一个 Bardwave 安装实例。它包含 `bundle.toml`，以及可选的音频文件（位于 `audio/` 下）。

### 资源包包含的内容

| 包含 | 不包含 |
|---|---|
| 分组 | 快捷键 |
| 声音：名称、分组、音量、重复、标签、增益、总线、短音效标记、声像 | 音频设备设置（输出和试听设备） |
| 场景和播放列表 | 响度归一化（normalization）和闪避（ducking）设置 |
| 音频文件，仅当勾选 **Include the audio files**（包含音频文件）时 | 配置方案 |

导出始终覆盖整个声音库，而不仅是筛选后可见的声音。没有音频的资源包，其声音会显示为 `File not found`，直到用户重新定位文件为止。

声音按相对于所有导出声音共有文件夹的路径保存（例如 `~/Music/Tavern/a.ogg` 和 `~/Music/Fx/b.wav` 会变为 `Tavern/a.ogg` 和 `Fx/b.wav`）。如果声音之间没有共同的文件夹，则只保留文件名。某些系统中不合法的字符会被替换。

最小清单文件示例（`bundle.toml`）：

```toml
format = "bardwave-bundle"
version = 1
name = "Tavern pack"

[[groups]]
id = "3f1c9a52-0d5e-4b8e-9a61-2c7d8e9f0a11"
name = "Tavern"

[[sounds]]
id = "8b2e4d10-7a3c-4f5e-b6d9-1e2f3a4b5c6d"
name = "Crowd"
rel_path = "Tavern/crowd.ogg"
audio = true
volume = 0.8
looped = true
group = "3f1c9a52-0d5e-4b8e-9a61-2c7d8e9f0a11"
tags = ["tavern"]
```

### Export

1. 点击搜索行中的 **Export…**（提示为 `Save the library as a shareable .zip bundle`）。
2. 在 **Export Bundle**（导出资源包）对话框中设置 **Bundle name**（资源包名称），默认值为 `Bardwave set`。
3. 保持勾选 **Include the audio files** 以包含音频；取消勾选则生成仅包含声音库数据的小型资源包。
4. 点击 **Choose File & Export…**（选择文件并导出）。保存对话框会建议文件名 `<name>.zip`；如果省略 `.zip`，会自动添加。声音库为空时，该按钮不可用。
5. 导出过程中可使用 **Cancel Export**（取消导出）；Esc 无法停止导出。已取消的导出不会写入任何内容，并返回选项页面。导出完成后，对话框会报告导出的声音数量和音频文件数量及其大小。点击 **Done**。

文件缺失的声音会在没有音频的情况下导出，对话框会在导出前后均给出警告。压缩包会先写入目标位置旁的临时文件，完成后再重命名，因此失败的导出绝不会留下损坏的资源包。同名的已有文件会被替换。

### Import

1. 点击 **Import…**，在标题为 `Choose a bundle to import` 的文件选择器中选择一个 `.zip`。
2. **Import Bundle**（导入资源包）对话框会检查该资源包。它会显示资源包名称、数量（`N sounds, N groups, N scenes, N playlists`，以及创建它的 Bardwave 版本），以及其中是否包含音频（`N audio files inside`，附带大小）。
3. 在 **If something already exists**（已有项目时）下选择如何处理已有项目：

| 选项 | 处理方式 | 行为 |
|---|---|---|
| **Keep mine**（保留我的） | 跳过（默认） | 保留已有项目。资源包中的副本不会被导入，资源包中的场景和播放列表会改为指向已有项目。若某声音的唯一冲突是目标文件夹中已存在的文件，则会登记到该文件上，该文件不会被覆盖 |
| **Keep both**（保留两者） | 重命名 | 导入的副本会获得一个未占用的名称，例如 `Tavern (2)`（文件变为 `hit (2).wav`）；如果其 ID 已被占用，则会分配新的 ID。同一资源包导入两次会创建副本 |
| **Use the bundle's**（使用资源包中的） | 替换 | 资源包中的内容会替换已有内容。项目保留自己的 ID，因此快捷键和场景仍然有效。目标文件夹中已有的音频文件会被覆盖 |

4. 设置 **Sound files go to**（声音文件保存位置），或点击 **Browse…**（文件选择器标题为 `Choose the folder for the sounds`）。默认文件夹为数据目录中的 `bardwave-library/imported/<bundle name>`。
5. 对话框会显示 **This import would**（本次导入将）：新增、重命名、替换和跳过的项目，以及将要写入的音频文件。此预览为试运行，不会改动任何内容。更改处理方式以及离开文件夹输入框时，预览会更新。
6. 点击 **Import**（导入）；文件夹输入框为空或预览显示错误时，该按钮不可用。导入过程中可使用 **Cancel Import**（取消导入）。最后点击 **Done**。搜索行下方会显示消息 `Bundle imported`。

在以下情况下，项目会被视为冲突：具有相同 ID；分组、场景或播放列表的名称相同（不区分大小写）；声音具有相同路径；或者对于带音频的声音，目标文件已存在。

只有在所有文件都写入完成后，声音库才会更改。如果导入失败或被取消，会删除已创建的文件。在 **Use the bundle's** 下被覆盖的文件无法恢复。

> **警告：** **Use the bundle's** 会覆盖目标文件夹中已有的音频文件。如果这些文件很重要，请先备份该文件夹。

### 安全限制

资源包可能来自任何人，因此导入器将其视为不受信任的内容。

- 含有绝对路径、`..` 或 `.`、空组成部分、反斜杠、冒号、控制字符或符号链接的条目会导致整个压缩包被拒绝。`bundle.toml` 中写入的声音路径同样适用此严格检查。消息以 `Import blocked for safety` 开头，且不会改动任何内容。
- 声音文件只会写入目标文件夹内（同时还会检查每个文件夹的真实位置）。只会写入 `wav`、`flac`、`mp3`、`ogg`、`m4a` 和 `aac` 文件。其他文件会被跳过并给出警告，该声音则会在没有音频的情况下导入。
- 大小限制：同时按声明的大小和实际读取的字节数检查。超出限制的资源包会以相同的 `Import blocked for safety` 消息被拒绝：

| 限制 | 默认值 |
|---|---|
| 压缩包中的条目数 | 20,000 |
| 分组、声音、场景和播放列表的总数 | 100,000 |
| 未压缩的 `bundle.toml` | 16 MiB |
| 单个音频文件的未压缩大小 | 2 GiB |
| 未压缩总大小 | 8 GiB |

面板使用这些默认值。它们在 GUI 中不可设置。

没有 `bundle.toml` 的 zip 会被拒绝，并显示 `This zip is not a Bardwave bundle`。来自更新格式版本的资源包会被拒绝，并提示更新 Bardwave。

<a id="headphone-preview"></a>
## 耳机试听

每行都有一个耳机按钮。点击它即可仅在本机上播放该声音，玩家听不到。再次点击即可停止。点击另一个声音的按钮会将试听切换到该声音。

- 提示为 `Preview in your headphones (players do not hear it)`。
- 对于缺失的文件，该按钮不可用，其提示为 `Preview is not available for this sound`。
- 试听播放时，提示为 `Stop preview`。
- 试听只播放一次（不重复），音量为该声音的音量（包括其响度增益）。试听会忽略该声音的声像和总线。
- **Stop All**（全部停止）也会停止试听，其他全部停止的途径（快捷键、定时器、远程控制）同样如此。
- 如果桌面音频输出未运行，消息会说明试听仅在本机播放，并注明输出状态。

试听输出在右侧列的 **AUDIO OUTPUT**（音频输出）部分的 **Preview** 下拉列表中设置。默认项为 `Same as output`。在配置文件中，对应的配置项是 `[audio]` 中的 `preview_device`。试听内容永远不会进入玩家的音频流。

> **注意：** 将试听路由到独立声卡的做法尚未在真实硬件上验证。

<a id="loudness-normalization"></a>
## 响度归一化

响度归一化会使用 EBU R128 对每个声音测量一次，并保存一个固定增益，使其达到目标响度。默认关闭，因此现有声音库的播放效果与以前相同。

在右侧列的 **MIX**（混音）部分（与网络控件在一起），勾选 **NORMALIZE LOUDNESS**（响度归一化）复选框即可开启。**Target**（目标响度）输入框以 LUFS 设置目标值，接受的范围为 -36 到 -8 LUFS。

状态行显示以下之一：

- `Off: sounds keep their own level`
- `On: new sounds are measured and set to -18 LUFS`（数字即为目标值）
- `Measuring N sounds...`

工作方式如下：

- 综合响度和真峰值在后台线程中对解码后的音频测量。短于 400 ms 的声音和静音声音不会被测量。
- 增益限制在 -24 dB 到 +12 dB 之间。提升增益受到限制，以确保真峰值保持在 -1 dBTP 以下。
- 增益以 `gain_db` 的形式保存在声音库中。声音的实际音量为 `volume × 10^(gain_db / 20)`，上限为 1.0。音量设置不会被改写。
- 由于音量上限为 1.0，只有当音量低于 1.0 时，正增益才会被听到。大多数响亮的素材需要负增益。
- 开启响度归一化后，所有已加载但尚无增益的声音（`gain_db = 0.0`）都会被测量，之后添加的每个声音也会被测量。已保存增益的声音不会再次被测量。
- 更改目标值不会重新测量已有增益的声音。若要重新测量这类声音，需在 `config.toml` 中将其 `gain_db` 重新设为 `0.0` 并重新启动（未验证）。
- 关闭响度归一化会保留已保存的增益，且这些增益会继续生效。
- 流式文件（见下文）不会被测量。

<a id="streaming-long-files"></a>
## 长文件流式读取

时长超过 `stream_threshold_secs`（默认 60 秒）的文件不会加载到内存中。它会在工作线程上解码，并从约 3 秒的环形缓冲区中播放。这样可以降低内存占用：一小时的立体声音频在内存中约占 1.4 GB。

- 短文件保留在内存中，因此可以立即开始播放。
- 如果工作线程落后，缺失部分的音频会以静音播放，并计为一次缓冲区欠载（underrun）。音频线程不会停止。
- 重复播放对流式文件同样有效。流式源也支持定位；声音库的行中没有定位控件。
- 耳机试听对流式文件同样有效。
- 设置 `stream_threshold_secs = 0` 可关闭流式读取。

如果无法从文件头获知时长（某些 MP3 文件），则由文件大小决定：大小超过阈值秒数乘以每秒约 16 kB 的文件会以流式读取。

<a id="configuration-reference"></a>
## 配置参考

声音库数据位于 `[[library.groups]]` 和 `[[library.sounds]]` 下。音频设置位于 `[audio]` 下。以下各值为代码中的默认值。其余配置项参见 [配置参考](./configuration.md)。

```toml
[audio]
normalize = false              # measure sounds without a gain_db and store one
normalize_target_lufs = -18.0  # target loudness, in LUFS
stream_threshold_secs = 60     # longer files are streamed; 0 = never
# preview_device = "Headphones"  # absent = previews use the main output

[[library.groups]]
id = "3f1c9a52-0d5e-4b8e-9a61-2c7d8e9f0a11"
name = "Tavern"

[[library.sounds]]
id = "8b2e4d10-7a3c-4f5e-b6d9-1e2f3a4b5c6d"
path = "/Users/me/Music/Tavern/crowd.ogg"
name = "Crowd"
volume = 0.8                   # 0.0 to 1.0
looped = true
group = "3f1c9a52-0d5e-4b8e-9a61-2c7d8e9f0a11"
tags = ["tavern", "crowd"]
pan = -0.25                    # -1.0 (left) to 1.0 (right)
gain_db = 0.0                  # written by normalization
bus = "ambience"               # "music", "ambience" or "sfx"
oneshot_stinger = false
```

| 配置项 | 所在节 | 默认值 | 取值范围或含义 |
|---|---|---|---|
| `normalize` | `[audio]` | `false` | 开启响度归一化 |
| `normalize_target_lufs` | `[audio]` | `-18.0` | 目标响度。**Target** 输入框接受 -36 到 -8 LUFS |
| `stream_threshold_secs` | `[audio]` | `60` | 秒；`0` 表示关闭流式读取 |
| `preview_device` | `[audio]` | 未设置 | 试听所用的设备名称。未设置时使用主输出 |
| `id` | `[[library.sounds]]` | 自动生成 | 稳定的 ID。缺失时自动生成 |
| `path` | `[[library.sounds]]` | 必填 | 音频文件的路径 |
| `name` | `[[library.sounds]]` | 文件名 | 显示名称。为空时回退到文件名 |
| `volume` | `[[library.sounds]]` | `1.0` | `0.0` 到 `1.0` |
| `looped` | `[[library.sounds]]` | `false` | 重复播放 |
| `group` | `[[library.sounds]]` | 未设置 | 分组 ID |
| `tags` | `[[library.sounds]]` | `[]` | 小写，最多 32 个字符 |
| `pan` | `[[library.sounds]]` | `0.0` | `-1.0` 到 `1.0`；居中时不写入 |
| `gain_db` | `[[library.sounds]]` | `0.0` | 由响度归一化写入。加载声音库时，取值限制在 -48 到 +24 dB 之间 |
| `bus` | `[[library.sounds]]` | `"ambience"` | `"music"`、`"ambience"` 或 `"sfx"` |
| `oneshot_stinger` | `[[library.sounds]]` | `false` | 作为单次播放叠加在场景之上，不会停止其他声音 |

单个声音的 `pan` 和 `volume` 值由 GUI 写入。配置文件只是存储位置；修改这些值需使用面板。

关于玩家页面的远程访问，参见 [HTTPS（安全连接）](./https.md)。关于 YouTube 下载问题、缺失文件和资源包导入安全的更多内容，参见 [故障排查、常见问题（FAQ）与安全](./troubleshooting-security.md)。

<a id="troubleshooting"></a>
## 故障排查

| 现象 | 可能原因 | 处理方法 |
|---|---|---|
| 琥珀色横幅 `N files not found` | 文件已被移动，或驱动器未挂载 | 挂载驱动器并重新启动 Bardwave（参见 [缺失文件](#missing-files) 中的说明）。若文件夹已被移动，使用 **Replace Path Prefix…**。若是单个文件，点击 **Locate…** |
| 导入后声音立即显示 `File not found` | 资源包中没有音频 | 在每一行上点击 **Locate…**，或导入导出时包含音频的资源包 |
| `Not a YouTube video link` | 播放列表页面、频道或其他网站 | 粘贴单个视频链接 |
| `ffmpeg not found on this system.`（macOS、Linux） | 未安装 ffmpeg | 使用包管理器安装 ffmpeg，然后重试 |
| 首次 YouTube 下载很慢 | 正在下载 yt-dlp 和 ffmpeg | 稍候。之后的下载会复用这些工具 |
| **YouTube** 按钮没有反应 | 仍有下载未完成（一次只能下载一个） | 等待其完成 |
| 文件已添加，但没有出现行 | 解码器无法读取该文件（例如 `.opus` 或 `.m4a`） | 在日志中查找 `Failed to load sound`。将文件转换为 WAV、FLAC、MP3 或 OGG |
| `That file is not a supported audio file` | 扩展名错误 | 选择扩展名受支持的文件 |
| `That file is already used by another sound in the library` | 所选文件属于另一个条目 | 先移除重复的条目 |
| `No sound path starts with that prefix.` | 旧前缀与保存的路径不匹配 | 从声音的提示中复制路径的准确开头 |
| `Import blocked for safety` | 压缩包含有不安全的条目，或超出大小限制 | 不要导入。向发送方索取新的导出文件 |
| 试听不可用 | 文件缺失 | 先重新定位该文件 |
| 长文件的响度归一化没有任何效果 | 文件以流式读取，因此不会被测量 | 如果必须测量该文件，提高 `stream_threshold_secs` |
| 提升增益后的声音并未变大 | 音量上限为 1.0 | 先降低音量，或接受负增益 |
| `NO MATCHES` | 搜索或标签筛选排除了所有内容 | 点击 **Clear Filters** |
