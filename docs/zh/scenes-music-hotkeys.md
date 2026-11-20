# 场景、音乐与快捷键

**场景**是保存下来的声音混音，包含各声音的音量及淡入淡出设置。**播放列表**是背景音乐。**发射器**（emitter，按随机间隔播放声音的声源）会在场景运行时随机播放声音。**定时器**会在稍后触发动作。**快捷键**通过键盘触发这些功能。

本页根据源代码及其单元测试编写。未在真实硬件上检查的行为标注为 **（未验证）**。关于 `config.toml` 的文件格式，请参阅 [配置参考](./configuration.md)；关于声音、短音效（stinger）和音频格式，请参阅 [声音库](./soundboard-library.md)。

## 目录

- [场景](#scenes)
- [停止声音](#stopping-sound)
- [发射器与场景音乐](#emitters-and-scene-music)
- [音乐播放列表](#music-playlists)
- [定时器](#timers)
- [总线、闪避与响度归一化](#buses-ducking-and-normalization)
- [快捷键](#hotkeys)
- [配置参考](#configuration-reference)
- [故障排查](#troubleshooting)

<a id="scenes"></a>
## 场景

场景只需一次操作，就能启动一组声音。激活场景时，不属于该场景的声音会淡出，除非 **Stop other sounds** 处于关闭状态（`stop_others = false`）。以下内容始终保留，不会被淡出：正在播放的短音效、该场景自身的发射器声音、该场景自身播放列表的曲目，以及该场景中标记为 **Manual**（手动）的声音。

再次激活已经处于活动状态的场景，不会重新启动任何内容。场景中已在播放的声音只会重新调整为该场景的音量与循环设置，仍在等待中的延迟启动保留其剩余时间。同一场景中出现两次的声音只计算一次（以第一项为准）。声音库中缺失声音的场景条目会被跳过，并记录在日志中。

### 当前混音的保存

1. 启动声音并设置其音量。
2. 在 **Scenes**（场景面板）中点击 **Save Mix As Scene**（将混音保存为场景）按钮；窗口较窄时显示为 **Save Mix**（保存混音），最窄时仅显示图标。
3. 在 **Scene name**（场景名称）中输入名称，最多 64 个字符。
4. 点击 **Save Scene**（保存场景）。

场景会保存当前正在播放的声音，并保留其当前音量与循环设置。不包括短音效。每个场景条目都会自动开始，没有延迟，且不覆盖声像（pan）。淡入淡出时间为 2000 ms，曲线为等功率，且 `stop_others` 开启。没有任何声音播放时，按钮处于不可用状态。名称重复（比较时不区分大小写）时会被拒绝，并显示 `A scene with this name already exists`。

### 场景面板与场景菜单

- 标题栏显示场景数量和活动场景。**Hide**（隐藏）/ **Show**（显示）用于折叠面板。**New**（新建）会创建一个空场景（并打开编辑器）。
- 点击场景卡片即可激活该场景。激活中的卡片显示 `ACTIVE`，淡入过程中显示 `ACTIVE · FADING IN n%`。卡片上的 `!` 徽标会列出问题，例如缺失的声音或文件。卡片上的铅笔图标会打开编辑器。
- 右键单击卡片，可选择 **Activate**（激活）、**Edit…**（编辑）、**Duplicate**（复制）或 **Delete…**（删除）。**Duplicate** 会复制条目、发射器、音乐和淡入淡出时间，并命名为 `X copy`。**Delete…** 会要求确认（**Delete**（删除）/ **Keep**（保留）），声音保留在声音库中，并移除启动该场景的快捷键。指向该场景的定时器不会被移除；其所在行会显示问题，触发时会记录警告。
- 右键单击声音列表中的某个声音，在 **Add to scene**（添加到场景）下选择场景，即可按已保存的音量将其添加到该场景。

### 场景编辑器

| 输入框 | 默认值 | 说明 |
| --- | --- | --- |
| Scene name | `Scene N` | 最多 64 个字符，不区分大小写时须唯一。 |
| Button colour（按钮颜色） | 无 | 八种预设色板、**None**（无），或 **Hex**（十六进制）输入框（`#rgb` / `#rrggbb`；缺少 `#` 时会自动补上）。深色会在卡片上提亮，以保证其可见。 |
| Fade in (ms), Fade out (ms)（淡入、淡出） | 2000 | 0 到 60000。可输入 `2000`、`2000 ms`、`2.5 s`。 |
| Curve（淡变曲线） | 等功率 | `Equal power`（在交叉淡化（crossfade）时总功率恒定）或 `Linear`。 |
| Stop other sounds: on / off | 开 | 开启或关闭 `stop_others`。 |

通过 **Add sound…**（添加声音）选择声音。每个条目都有一个音量滑块（条目的音量会替代该声音在场景内的自身音量）、**Loop**（循环）、启动方式（**Auto**（自动）或 **Manual**）、一个声像滑块、一个延迟输入框（默认 `0`，最大 600000 ms，可输入 `1.5 s`）以及一个移除按钮。新条目会采用该声音自身的音量和循环标记。

- **Auto**（`on_enter`）会在场景激活时启动该条目，并在其延迟结束后开始播放。
- **Manual**（`manual`）会列出该条目，但只有手动启动（例如从声音列表中启动）时才会开始播放。其延迟会被忽略，且该输入框不可用。
- 声像滑块范围为 L 100% 至 R 100%；双击可将其居中。条目的非零声像会在该场景激活期间替代声音自身的声像；居中则表示使用声音自身的声像。
- 延迟启动由 Director 保存，并在其调度周期（tick）中释放。若在延迟结束前离开该场景，则取消该启动。

发射器与场景的音乐播放列表在单独的对话框中编辑（参见 [发射器与场景音乐](#emitters-and-scene-music)）；在场景编辑器中保存不会改动它们。

### 过渡时间

**Transition**（过渡）一行包含一个 **Auto** 开关和一个滑块，范围为 0 到 10 s，步长 100 ms，初始值为 2.0 s。开启 **Auto** 时，该行显示 `Per scene`，每个场景使用其自身的淡入淡出时间。移动滑块会关闭 **Auto**，并将该时间同时用作场景卡片、**Stop Scene**（停止场景）与 **Fade Out**（淡出）的淡入和淡出时间。滑块的值不会保存到 `config.toml`，场景本身也不会改变。

> **注意：** 快捷键和定时器会忽略该滑块，而使用各场景自身的淡入淡出时间。此内容通过阅读代码得出，未实际运行。

### 短音效

短音效是叠加在所有其他声音之上的单次播放声音。它不会停止其他声音，并且在播放期间，场景切换不会停止它。右键单击声音，选择 **Use As Stinger**（用作短音效）或 **Remove Stinger Flag**（移除短音效标记）。每个短音效都会在场景网格下方的 **Stingers**（短音效）行中有一个触发键；该触发键显示声音名称，并播放一次。短音效不属于已保存的混音。短音效在声音所属的总线（bus）上以该声音自身的音量播放。在 `config.toml` 中，该标记为 `oneshot_stinger = true`。为短音效声音设置的快捷键，以及定时器动作 **Play stinger**（播放短音效），会以相同方式触发它。

<a id="stopping-sound"></a>
## 停止声音

| 控件 | 停止对象 | 淡出时间 |
| --- | --- | --- |
| **STOP ALL**（全部停止；顶栏中的紧急停止按钮） | 所有内容同时停止：声音、场景、音乐和耳机试听（headphone preview）。 | 无（仅有短暂的防咔嗒渐变）。 |
| **Stop Scene**（面板；无活动场景时不可用） | 活动场景中正在播放的声音、其发射器，以及该场景启动时引入的音乐。短音效和其他声音继续播放。 | 若 **Auto** 关闭，则使用滑块的值；否则使用场景的 `fade_out_ms`。 |
| **Fade Out**（面板） | 所有内容，包括短音效、发射器和音乐。等待中的延迟启动会被取消。 | 若 **Auto** 关闭，则使用滑块的值；否则使用活动场景的 `fade_out_ms`，没有活动场景时为 2000 ms。 |
| 快捷键 `Stop scene` | 与 **Stop Scene** 相同。 | 场景自身的 `fade_out_ms`（忽略滑块）。 |
| 快捷键 `Stop all` | 所有内容。耳机试听也会停止。 | 活动场景的 `fade_out_ms`；没有活动场景时为 0 ms。 |
| 定时器 `Stop all (fade out)` | 所有内容。耳机试听也会停止。 | 动作的 `fade_ms`（整数秒，控制台中范围为 0 到 120）。 |

执行 **Fade Out**、**STOP ALL** 或 `Stop all` 之后，不再有活动场景。**Stop Scene** 之后同样不再有活动场景。

<a id="emitters-and-scene-music"></a>
## 发射器与场景音乐

打开底部音乐栏中的 **Music…**（音乐），然后选择 **Scenes** 选项卡。在某个场景行上点击 **Emitters & music…**（发射器与音乐）。该对话框用于编辑场景的发射器和音乐；点击 **Save**（保存）应用更改（在没有任何更改，或某个发射器没有声音时，该按钮不可用），点击 **Close**（关闭）则退出。

- **Music playlist**（音乐播放列表）用于选择一个播放列表，或选择 `None`（在 `config.toml` 中为 `playlist = "<id>"`）。激活场景时会启动该播放列表，淡入时间取场景淡入时间与播放列表的交叉淡化时间中较长的一个。停止场景时会将其淡出。详情：
  - 场景激活时已在播放的播放列表不会重新启动。
  - 未设置播放列表的场景会停止由先前场景引入的音乐，但不会停止手动启动的播放列表。
  - 开启 **Stop other sounds** 的场景，会淡出手动启动的、且不属于该场景自身的播放列表。
  - **Stop Scene**、**Fade Out**、**STOP ALL** 以及对应的快捷键，会随场景一同结束该场景的音乐。
  - 缺失的播放列表会予以报告，该场景的音乐层随之停止。
- **Add emitter**（添加发射器）会添加一个随机声源。每个发射器包含以下设置：

| 设置项 | 默认值 | 含义 |
| --- | --- | --- |
| Every … to …（间隔） | 20 s 到 60 s | 最短与最长间隔。控制台允许的范围为 0.1 s 到 1 h。最小值高于最大值时，最大值会随之调整，反之亦然。 |
| Volume（音量） | 100% | 基础音量。 |
| Volume jitter（音量随机偏移） | 0% | 音量的随机波动：`volume x (1 + jitter x u)`，`u` 为 -1 到 1 之间的随机数，结果保持在 0 到 1 之内。 |
| Pan jitter（声像随机偏移） | 0% | 随机立体声位置：`jitter x u`；0 表示居中，100% 时可能落在任意位置。 |
| Max overlap（最大同时播放数） | 2 | 该发射器同时存在的实例数，控制台中为 1 到 16。 |
| Sounds（声音） | 无 | 每次播放时随机选取一个，优先选择未在播放的声音。至少需要一个声音。 |

行为：

- 首次播放会在最多 5 s 的随机偏移后开始（不晚于最长间隔），因此场景开始时不会出现集中播放。
- 每个发射器都会在其两个限值之间随机等待。调度器的触发间隔不会短于 100 ms。
- 一次播放持续的时间与其声音相同（长度未知时按 5 s 计算）。若到了播放时间而所有重叠槽位都已占用，则跳过该次播放，并重新抽取一个间隔。
- 发射器在每个调度周期中最多触发一次，下一次间隔从该调度周期开始计算，因此应用卡顿后不会集中补播。
- 再次激活正在运行的场景会保留发射器的定时器。发射器随场景一同停止。场景停止时，仍在发声的实例会按场景的淡出时间淡出；若再次激活同一场景，则这些实例会保留下来。
- 场景运行期间，每张发射器卡片会显示实时倒计时（`Next in 12 s`，以及 `n playing`）。若场景未运行，则显示 `Scene not active`；若没有可播放的声音，则显示 `No playable sounds`。
- 发射器在 Director 的调度周期上运行，不额外占用线程。引擎为每个声音保存一组音量和声像，因此在同一声音仍在播放时启动的实例，会沿用该声音的声像和音量。

> **注意：** 每次场景启动都使用随机种子（测试中固定种子）。在真实硬件上的播放效果 **（未验证）**。

<a id="music-playlists"></a>
## 音乐播放列表

播放列表是按顺序排列的声音库声音列表，在音乐总线（**Music**）上播放。它可以从音乐栏播放，也可以作为场景的音乐层播放。同一时间只有一个播放列表在播放；启动另一个播放列表会将其替换。

| 设置项 | 默认值 | 含义 |
| --- | --- | --- |
| Shuffle（随机播放） | 关 | 每一轮都会先让每首曲目播放一次，然后才会重复；新一轮绝不会以上一轮结束时的曲目开始。 |
| Repeat（重复） | All | `Off` 在结尾处停止（状态为 `Finished`）。`All` 从头重新播放列表。`One` 无间隔地循环当前曲目。按钮依次切换 Off、All、One。 |
| Crossfade（交叉淡化） | 2.0 s | `Off`（0），最长 30 s。下一首曲目会在当前曲目结束前 `crossfade` 的时间开始，但不会早于当前曲目的中点，因此两者会重叠。该功能需要已知的曲目长度；在 Repeat `One` 模式下，或在 Repeat `Off` 模式下的最后一首曲目上，该功能不生效。 |
| Bus (`bus`) | Music | `music`、`ambience` 或 `sfx`。仅在 `config.toml` 中设置；控制台没有对应控件。 |

随机播放、重复模式和交叉淡化设置会随播放列表一同保存。播放期间更改随机播放或重复模式，会立即生效。

音乐栏（控制台底部）包含 **Previous track**（上一曲目）、播放按钮（**Play the playlist**（播放该播放列表）/ **Pause**（暂停）/ **Resume**（继续），没有播放列表时为 **Create a playlist**（创建播放列表））、**Stop the music**（停止音乐）、**Next track**（下一曲目）、带时间显示的进度条、**Shuffle**（随机播放）、**Repeat Off / All / One**（重复）、当前曲目名称及其位置（`Track 3 / 12`），以及 **Music…** 按钮。若当前曲目已播放超过 3 s（或前面没有曲目），**Previous track** 会从头开始播放当前曲目；否则会切换到上一首曲目。**Stop the music** 会在播放列表的交叉淡化时间内淡出。下一曲目和上一曲目同样使用交叉淡化。

在 **Music…** 管理器（**Playlists**（播放列表）选项卡）中，**New playlist**（新建播放列表）会创建一个列表。管理器提供播放、停止、随机播放、重复和交叉淡化控件、一个重命名按钮，以及一个删除按钮（声音仍保留在声音库中）。在 **Add sounds**（添加声音）下，可按名称或标签筛选声音库，并通过 **+** 按钮将曲目添加到末尾。拖动手柄，或使用 **Move up**（上移）/ **Move down**（下移），可调整曲目顺序；使用 **Remove from the playlist**（从播放列表中移除）可移除曲目。从声音库中删除声音，也会将其从播放列表中移除。

- 只有在曲目正在播放或暂停、且其长度已知时，才能定位；定位位置绝不会落在结尾前 500 ms 之内。
- 名称依次取自文件元数据（格式为 `Artist - Title`，或仅 `Title`）、声音名称，最后是 `Unknown track`。会读取专辑标签，但控制台不显示。
- 缺失或无法播放的曲目会被跳过并予以报告。若没有任何曲目可以播放，播放列表会停止并予以说明。若某曲目被其他操作停止（且并非接近结尾），则播放列表会随之停止，而不会跳到下一曲目。
- 长文件采用流式读取（参见 `stream_threshold_secs`）。解码使用 Symphonia，并启用了 `vorbis`、`mp3`、`flac`、`wav` 和 `aac` 这几个 Cargo feature；模块注释中还提到了 Opus，但 `Cargo.toml` 中未启用任何 Opus 解码器 feature，因此 Opus 播放 **（未验证）**。格式表请参阅 [声音库](./soundboard-library.md)。

<a id="timers"></a>
## 定时器

从“Mix”（混音）部分打开 **Timers**（定时器），即 **Duck**（闪避，ducking）和 **Hotkeys**（快捷键）下方的 **Timers** 按钮；定时器运行时，该按钮会显示下一个定时器的 `Timers MM:SS`。定时器仅在应用打开时运行，且始终以停止状态启动；仅保存定时器的定义。

- **Start**（开始）会将所有已启用的定时器置为待触发状态。暂停后，该按钮显示为 **Resume**。
- **Pause timers**（暂停定时器）会冻结倒计时。
- **Reset**（重置）会取消所有定时器的待触发状态。下一次 **Start** 将从零开始。
- 状态行显示 `Stopped`、`Running mm:ss` 或 `Paused mm:ss`。每个定时器行包含一个启用复选框、一个倒计时（`OFF`、`DONE`、`--:--`），以及编辑和删除按钮。底部的 **Fired**（已触发）列表会记录已执行的内容以及所有警告。

**New timer**（新建定时器）会打开一个表单，包含 **Name**（名称；可选，最多 60 个字符，留空时自动生成名称）、**When**（触发时机）、**Action**（动作）以及目标。**When** 可选 **Once, after**（一次，在……之后）或 **Repeat every**（每隔……重复），并设置分钟（0 到 999）和秒。间隔至少为 1 秒（配置文件本身可接受 100 ms）。新表单默认为 **Once, after** 10 min，动作为 **Activate scene**（激活场景）。已延迟的重复定时器只会触发一次，并报告错过的周期。

| 动作文字 | `kind` | 附加字段 |
| --- | --- | --- |
| Activate scene | `activate_scene` | `scene` |
| Stop scene | `stop_scene` | 无 |
| Play stinger | `play_stinger` | `sound`（声音库中的任意声音，仅播放一次） |
| Stop all (fade out) | `stop_all` | `fade_ms` |
| Start playlist | `start_playlist` | `playlist` |
| Stop music | `stop_playlist` | 无 |
| Next track / Pause music / Resume music | `next_track` / `pause_music` / `resume_music` | 无 |

指向缺失对象的定时器会记录警告，且不影响其他定时器；其所在行会显示 `Cannot run: …` 或类似 `Scene no longer exists in the library` 的说明。已禁用的定时器，或定义无效的定时器，永远不会进入待触发状态。编辑运行中的定时器时，未更改的定时器保留其倒计时。

<a id="buses-ducking-and-normalization"></a>
## 总线、闪避与响度归一化

每个声音都在一条总线上播放：**Music**、**Ambience**（氛围音）或 **Sfx**（音效）。声音的总线由其 `[[library.sounds]]` 条目中的 `bus` 决定，默认为 `ambience`；播放列表的默认值为 `music`。控制台中没有可更改这两者的控件；请编辑 `config.toml`。每条总线在“Mix”部分（网络列）中都有一个推子。推子的位置不会保存，重新启动后会恢复为 100%。

**Duck** 在激活期间会降低 `duck_buses` 中列出的总线音量（默认为 Music 和 Ambience），激活时按钮显示 **Ducked**（已闪避）。**Click**（点击）会将其锁定，直到下一次点击。**Hold**（按住）仅在按钮按下期间闪避（短于 350 ms 的按压视为点击）。锁定状态与按住的按键互不取消。折叠的 **Duck settings**（闪避设置）部分可设置：

| 设置项 | 配置项 | 默认值 | 控制台范围 |
| --- | --- | --- | --- |
| Level（音量） | `duck_amount` | `0.3`（约 -10 dB） | 0 到 100% |
| Attack（启动时间） | `duck_attack_ms` | `200` | 0 到 10000 ms |
| Release（释放时间） | `duck_release_ms` | `800` | 0 到 30000 ms |
| Ducks（闪避总线） | `duck_buses` | `["music", "ambience"]` | 三种总线中的任意几种 |

闪避不会改变推子的值：释放后，总线会精确恢复到推子设定的音量。`duck_amount = 1.0` 会禁用闪避，`0.0` 会将被闪避的总线静音。快捷键 `Duck background (toggle)` 和 `Duck background (hold)` 使用与按钮相同的设置和状态。

**NORMALIZE LOUDNESS**（响度归一化，normalization；`normalize`，默认关闭）会在后台使用 EBU R128 测量声音，并保存向 `normalize_target_lufs` 靠拢的 `gain_db` 校正值（**Target**（目标响度）输入框，默认 `-18.0`，控制台范围为 -36 到 -8）。它只测量已加载且 `gain_db` 为 `0` 的声音，每次运行只测量一次；测量得到的校正值限制在 -24 到 +12 dB 之间。更改目标值不会重写已保存的增益，只会使尚未测量的声音能够重新测量。长于 `stream_threshold_secs`（默认 60）的文件不会被测量。声音的实际音量等于其音量乘以增益，并保持在 0 到 1 之间，因此只有当音量低于 1.0 时，正增益才会起作用。开关下方的说明行会显示 `Off: sounds keep their own level`、`On: new sounds are measured and set to -18 LUFS` 或 `Measuring n sounds...`。在真实扬声器上的结果 **（未验证）**。

<a id="hotkeys"></a>
## 快捷键

在“Mix”部分中打开 **Hotkeys**。对话框列出已有的绑定（**Bindings (n)**，即绑定列表）；其中有一行 **New hotkey**（新建快捷键）：包含一个动作菜单，若动作需要目标，还有第二个目标菜单；随后是 **Capture key**（捕获按键；按 **Esc** 取消）或在输入框 `or type: Ctrl+Shift+1` 中输入的组合键（按 **Enter** 确认），以及 **Add hotkey**（添加快捷键）。点击列表中的按键即可更改它。某一行的删除按钮（**Delete this hotkey**（删除此快捷键））会移除该绑定。**Done**（完成）会关闭对话框。编辑器等待按键时，不会触发任何绑定。数字小键盘和 Win/Super 键无法捕获，请改为手动输入。

### 按键语法

零个或多个修饰键加一个按键，以 `+` 连接，不区分大小写，顺序不限。修饰键：`Ctrl`（`Control`）、`Alt`（`Option`）、`Shift`、`Cmd`（`Command`、`Super`、`Meta`、`Win`）。`CmdOrCtrl` 在 macOS 上表示 `Cmd`，在其他系统上表示 `Ctrl`。按键包括：`A` 到 `Z`、`0` 到 `9`、`F1` 到 `F24`、`Numpad0` 到 `Numpad9`（以及 `NumpadAdd`、`NumpadSubtract`、`NumpadMultiply`、`NumpadDivide`、`NumpadDecimal`、`NumpadEnter`）、`Space`、`Enter`、`Escape`、`Tab`、`Backspace`、`Delete`、`Insert`、`Home`、`End`、`PageUp`、`PageDown`、方向键 `Up`、`Down`、`Left`、`Right`，以及 `Minus`、`Comma`、`Slash` 等标点符号。控制台会写入规范形式（`Ctrl+Alt+Shift+Cmd+Key`）。

### 动作

| 菜单文字 | `kind` | 目标 |
| --- | --- | --- |
| Play sound / stinger | `sound` | 声音 id |
| Activate scene | `scene` | 场景 id |
| Start playlist | `playlist` | 播放列表 id |
| Stop all | `stop_all` | 无 |
| Stop scene | `stop_scene` | 无 |
| Master volume up / down | `master_up` / `master_down` | 无（步长 0.05） |
| Master mute | `master_mute` | 无 |
| Duck background (toggle) | `duck_toggle` | 无 |
| Duck background (hold) | `duck_hold` | 无 |
| Pause / resume music | `pause_music` | 无 |
| Next track | `next_track` | 无 |
| Previous track | `prev_track` | 无 |

声音快捷键会切换普通声音（以其自身音量开始，再按一次则停止），并以单次播放的方式触发短音效。“Master mute”会恢复之前的音量（若静音来自其他途径，则恢复为 0.8）。除 `Duck background (hold)` 外，所有动作都会忽略按键释放；若释放事件丢失（窗口失去焦点，或文本框接管了键盘），则会强制解除闪避。场景和声音的快捷键使用声音库中的当前数据，并使用各场景自身的淡入淡出时间。

### 本地与全局

- **Local**（本地）快捷键在 Bardwave 窗口获得焦点时触发。
- **Global**（全局）快捷键在后台也会触发。可通过 `[gui] global_hotkeys = true` 启用，或勾选 **GLOBAL HOTKEYS (WHEN THE WINDOW IS NOT FOCUSED)**（窗口没有焦点时的全局快捷键）复选框。默认关闭。对话框会显示状态（`Global hotkeys are off.`、`Global hotkeys active (n).`，或当系统拒绝注册某个按键时显示 `n could not be registered`，例如该按键已被其他程序占用）。
- 只有包含 `Ctrl`、`Alt` 或 `Cmd` 的组合键，以及 `F` 键，才能设为全局。`Space`、`A` 或 `Shift+1` 等普通按键不能设为全局快捷键。
- 文本框获得焦点时，普通按键和 `Shift+key` 会输入到文本框中，而不会触发；包含 `Ctrl`、`Alt` 或 `Cmd` 的组合键以及 `F` 键仍会触发。
- 窗口无法检测小键盘按键（`Numpad…`）：它们只能作为全局快捷键生效，因此需要修饰键并开启全局选项。`Cmd` 绑定仅在 macOS 上作为本地快捷键匹配（窗口无法检测 Windows/Super 键）。
- 已注册为全局的按键，在窗口获得焦点时同样会被接收，此时会跳过本地匹配，以免触发两次。
- macOS 可能需要在 **系统设置 > 隐私与安全性** 中授予权限（辅助功能或输入监控）；未授予时，快捷键仅在窗口获得焦点时生效。代码注释称普通按键无需权限，因此这一点 **（未验证）**。
- Windows：**（未验证）**。Linux：仅支持 X11，因为 Wayland 不允许全局快捷键。**（未验证）**

### 冲突与 MIDI

出现问题时，对话框会在相应行下方显示消息。若多个绑定使用同一按键，则第一个生效。

| 消息 | 含义 |
| --- | --- |
| `Conflict: the same key is bound in #N, which wins - this one never fires` | 先前的绑定生效。添加已绑定的按键时会提供 **Replace**（替换），它会替换该绑定，而不是添加重复项。 |
| `Conflict: the same key is also bound in #N, which never fire` | 此绑定优先于其后列出的绑定。 |
| `The sound, scene or playlist it points at no longer exists` | 目标已被删除。 |
| 无效的按键文本（例如 `…has two keys…`） | 无法解析该按键。 |

在控制台中删除声音、场景或播放列表时，指向它们的绑定也会一并删除。无法运行的快捷键（例如 `Hotkey ignored: the audio engine is not running`）会写入日志，动作运行时的错误则会显示在对话框中。

MIDI 绑定使用 `key = "Midi:Ch1:Note60"` 或 `key = "Midi:CC7"` 的格式，且只能在 `config.toml` 中编写。相关代码仅在启用 Cargo feature `midi` 时才会编译，即便如此，它也未与控制台相连。控制台会将其显示为 `MIDI binding: MIDI input is not connected in this build, it will not fire`。应将 MIDI 视为不可用 **（硬件上未验证）**。

<a id="configuration-reference"></a>
## 配置参考

场景、播放列表和声音位于 `[library]` 下。快捷键和定时器位于顶层。ID 是由控制台写入的 UUID 字符串。若手写的 ID 不是 UUID，会在加载文件时被替换，同时改写场景、发射器、播放列表和 `[[hotkeys]]` 中的引用；`[[timers]]` 不会被改写，因此这里要使用真实的 UUID。默认值：场景淡入淡出时间 `2000`；场景 `curve = "equal_power"`；`stop_others = true`；条目 `volume = 1.0`、`start = "on_enter"`、`delay_ms = 0`；发射器 `min_interval_ms = 20000`、`max_interval_ms = 60000`、`volume = 1.0`、`volume_jitter = 0`、`pan_jitter = 0`、`max_overlap = 2`；播放列表 `repeat = "all"`、`crossfade_ms = 2000`、`bus = "music"`；声音 `bus = "ambience"`；定时器 `enabled = true`。

```toml
[gui]
global_hotkeys = false

[[library.playlists]]
id = "6a0f3b1e-52c4-4f0e-9a52-0b1f5f3a7a01"
name = "Tavern music"
items = ["2f6c1d8a-7b3e-4c52-8d61-9e0a4b7c1d02", "2f6c1d8a-7b3e-4c52-8d61-9e0a4b7c1d03"]
shuffle = true
repeat = "all"
crossfade_ms = 3000

[[library.scenes]]
id = "8c2d9e44-1f6a-4b7d-a3c5-5e7f0a9b2c04"
name = "Tavern"
fade_in_ms = 2000
fade_out_ms = 2000
curve = "linear"
stop_others = true
playlist = "6a0f3b1e-52c4-4f0e-9a52-0b1f5f3a7a01"

[[library.scenes.items]]
sound = "2f6c1d8a-7b3e-4c52-8d61-9e0a4b7c1d05"
volume = 0.4
looped = true
start = "on_enter"

[[library.scenes.emitters]]
sounds = ["2f6c1d8a-7b3e-4c52-8d61-9e0a4b7c1d06", "2f6c1d8a-7b3e-4c52-8d61-9e0a4b7c1d07"]
min_interval_ms = 20000
max_interval_ms = 60000
max_overlap = 2

[[hotkeys]]
key = "Ctrl+1"
action = { kind = "scene", target = "8c2d9e44-1f6a-4b7d-a3c5-5e7f0a9b2c04" }

[[hotkeys]]
key = "F5"
action = { kind = "stop_all" }

[[timers]]
id = "b13e5f70-3a8d-4e19-b6c2-7d4a9e0f1c08"
name = "Alarm in ten minutes"
trigger = { after_ms = 600000 }
action = { kind = "activate_scene", scene = "8c2d9e44-1f6a-4b7d-a3c5-5e7f0a9b2c04" }
enabled = true

[[timers]]
id = "c24f6a81-4b9e-4f2a-87d3-8e5bab1f2d09"
name = "Bell every half hour"
trigger = { every_ms = 1800000 }
action = { kind = "play_stinger", sound = "2f6c1d8a-7b3e-4c52-8d61-9e0a4b7c1d0a" }
enabled = true
```

`[audio]` 下的音频设置：`duck_amount`（0.3）、`duck_attack_ms`（200）、`duck_release_ms`（800）、`duck_buses`（`["music", "ambience"]`）、`normalize`（false）、`normalize_target_lufs`（-18.0）、`stream_threshold_secs`（60，`0` 会关闭流式读取）。

> **警告：** 控制台保存时会改写 `config.toml`。手动编辑该文件之前，请先关闭 Bardwave。

> **注意：** 未写入 `fade_ms` 的 `stop_all` 定时器会以 `0` ms 淡出。控制台中新建定时器的默认值为 2 s。

<a id="troubleshooting"></a>
## 故障排查

| 现象 | 可能原因与解决方法 |
| --- | --- |
| **Save Mix As Scene** 不可用 | 没有任何声音在播放。先启动一个声音。 |
| **Stop Scene** 不可用 | 没有活动场景。 |
| 快捷键没有任何反应 | 存在冲突、目标已被删除，或按键无效。请查看快捷键对话框中的消息以及日志。 |
| 普通按键仅在窗口中生效 | 普通按键从不作为全局快捷键。请添加修饰键。 |
| 全局快捷键没有任何反应 | 未开启该选项、缺少权限，或当前为 Wayland 会话。**（未验证）** |
| 定时器从不触发 | 定时器未启动，或已被禁用。点击 **Start**。 |
| 发射器无声 | 已达到重叠上限、场景未激活，或没有声音。 |
| 播放列表中的曲目被跳过 | 文件缺失或无法启动。 |
| 进度条没有任何反应 | 曲目长度未知，或没有任何曲目在播放。 |
| Fade Out 忽略滑块 | **Auto** 已开启。请移动滑块。 |
| 快捷键或定时器的淡出时间不对 | 它们使用各场景自身的淡入淡出时间，而非 Transition 滑块。 |

## 覆盖范围

已根据源代码和单元测试验证：场景、发射器、定时器、播放列表和闪避逻辑，快捷键解析与分发，以及控制台上的文字。未验证：真实硬件上的音频输出、macOS、Windows 和 Linux 上的全局快捷键、MIDI、Opus 播放以及安装程序。关于远程页面的 HTTPS 设置，请参阅 [HTTPS（安全连接）](./https.md)。
