# 网络、玩家与主持人远程面板

Bardwave 通过局域网（LAN）或互联网，将控制台的输出以音频流的形式发送给听众。本页介绍服务器如何启动和访问、玩家如何凭访问密钥加入、听众页面显示什么，以及主持人（GM）如何通过远程面板从另一台设备操作控制台。HTTPS 的设置请参见 [HTTPS（安全连接）](./https.md)。相关页面：[快速入门](./getting-started.md)、[配置参考](./configuration.md)、[故障排查、常见问题（FAQ）与安全](./troubleshooting-security.md)。

涉及两项机密。**访问密钥**允许玩家收听。**主持人令牌**允许某人控制控制台。访问密钥永远无法打开主持人面板。

## 目录

- [启动与停止服务器](#starting-and-stopping-the-server)
- [局域网或互联网](#lan-or-internet)
- [UPnP 端口转发](#upnp-port-forwarding)
- [玩家链接与访问密钥](#player-link-and-access-key)
- [听众页面](#listener-page)
- [主持人远程面板](#gm-remote-panel)
- [无头模式下的 /control 页面](#headless-control-page)
- [HTTPS](#https)
- [配置参考](#configuration-reference)
- [现象与解决方法](#symptoms-and-fixes)
- [未验证](#not-verified)

<a id="starting-and-stopping-the-server"></a>
## 启动与停止服务器

服务器不会随应用一起启动。主持人在 **Network Server**（网络服务器）面板中启动它（该面板位于控制台右侧一列，标题为 `02 / Network`）。在 `--server-only` 模式下没有窗口，服务器会自行启动，参见[无头模式下的 /control 页面](#headless-control-page)。

1. 服务器停止时，检查面板底部 **Configuration**（配置）区块中的 **Port**（端口）和 **Bind Address**（绑定地址）。
2. 点击 **Start Server**（启动服务器）。
3. 点击 **Copy Link**（复制链接），或点击 **Listen URL**（监听地址）旁边的复制图标，然后把链接发送给玩家。
4. 点击 **Stop Server**（停止服务器），让音频流停止广播。

状态卡片以大写字母显示当前状态：

| 状态 | 含义 |
| --- | --- |
| `Live` | 运行中。其下显示：`Broadcasting to N listeners`。 |
| `Starting` | `Binding the listen socket`。按钮不可用。 |
| `Error` | 上一次启动失败。原因会显示出来，并截短为 34 个字符，完整文本写入日志。按钮变为 **Retry Server**（重试服务器）。 |
| `Off Air` | 已停止。其下显示：`Server stopped`。 |

**Listeners**（听众）区块显示当前数量以及 `Peak`（自服务器启动以来的最高数量）。

**Port** 接受 1 到 65535。端口 `0` 会被拒绝（`Port must be 1–65535`）。**Bind Address** 只接受 IP 地址（IPv4 或 IPv6），不接受主机名（`Bind must be an IP address`）。无效输入会保留上一个有效值，并在输入框下方显示错误。有效的修改会立即生效，并保存到 `config.toml`（会出现 `Saved` 标记）。两个输入框只能在服务器停止时编辑；其下的提示显示 `Editable while stopped` 或 `Stop the server to edit`。

停止服务器会断开所有听众和所有远程主持人会话，移除路由器上的端口映射，并清除 VPN 与防火墙提示。已打开的听众页面会持续重试，并在服务器再次启动后重新加入。

<a id="lan-or-internet"></a>
## 局域网或互联网

对外公布的地址取决于绑定地址：

| 绑定地址 | 对外公布的地址 |
| --- | --- |
| `0.0.0.0` 或 `::`（默认为 `0.0.0.0`） | 检测到的、与绑定地址同一协议族的局域网地址。若未检测到，则使用环回地址（`127.0.0.1` 或 `[::1]`）。 |
| 环回地址（`127.0.0.1`、`::1`） | 仅环回地址可用。只有本机可以连接。 |
| 其他任意 IP 地址 | 即所输入的地址。 |

检测到地址时，私有局域网地址优先于公网 IP。在 IPv4 局域网地址中，`192.168.x.x` 优先于 `10.x.x.x`，后者又优先于 `172.16.0.0/12`（Docker、WSL 以及许多 VPN 适配器所在的网段）。基准测试地址段（`198.18.0.0/15`，部分 VPN 和代理适配器使用）、链路本地地址、环回地址以及其他不可用的地址段都不会被选中。运营商级 NAT 地址（例如 Tailscale 的 `100.64.0.0/10`）是回退到环回地址之前的最后选择。

同一网络中的玩家使用局域网链接。其他位置的玩家需要 **Internet**（互联网）链接，该链接只有在路由器转发端口时才会出现，参见[UPnP 端口转发](#upnp-port-forwarding)。

**Listen URL** 区块显示对外公布的链接以及访问密钥（屏幕上会截短为 30 个字符）。当对外公布的地址不是环回地址时，其下方的 `Local` 行会显示本机的环回链接。点击该行即可复制带访问密钥的链接。

> **警告：** 访问密钥会随 URL 一起传输。在通过互联网向玩家发送链接之前，请启用 [HTTPS](#https)。

<a id="upnp-port-forwarding"></a>
## UPnP 端口转发

当 `upnp = true`（默认值）时，服务器会请求路由器转发监听端口（TCP，UPnP IGD）。该请求在后台运行，不会延迟启动。局域网链接会立即出现；只有当路由器授予映射时，才会多出一行互联网链接。发现请求从本机的每个私有 IPv4 地址发出，因此即使 VPN 占用了默认路由，它也能工作（在真实路由器上未验证）。

| Listen URL 区块中的行 | 含义 |
| --- | --- |
| `Internet: asking the router…` | 正在询问路由器。 |
| `Internet http://<public-address>:<port>` | 指向公网 IP 的映射（启用 HTTPS 时为 `https://`）。点击可连同访问密钥一起复制。当路由器上的另一台机器已经转发了该端口时，外部端口可能与本地端口不同。 |
| `Internet: blocked by a second NAT (<address>)` | 路由器的外部地址为私有地址。 |
| `Internet: no UPnP router` | 没有路由器应答，或没有路由器授予映射。将鼠标悬停在该行上可查看最近一次的原因。服务器每 30 秒重试一次。 |

完全没有 `Internet` 行，表示端口转发已关闭或不适用（参见下文的 **环回地址或 IPv6 绑定**）。

注意事项：

- **双重 NAT。** 运营商级 NAT，或路由器前面的运营商调制解调器，会使外部地址成为私有地址。请向运营商申请公网 IP，或在上游设备上转发该端口。
- **VPN。** 当全隧道 VPN 占用默认路由时，路由器仍会转发连接，但应答会经由隧道发出。此时面板会显示 `VPN active (<address>): internet players can't connect until it is off`。请关闭 VPN，或将 Bardwave 排除在 VPN 之外。当默认路由地址与对外公布的 IPv4 局域网地址不同时，会触发该警告。
- **防火墙。** 在 Windows 上，服务器会尝试为该端口添加入站允许规则（`netsh`，适用于所有网络配置文件），这需要管理员权限。如果失败，面板会显示 `Firewall: port <port> not opened - allow Bardwave for public networks or run as admin`。Windows 常将网络标记为公用网络，因此也请在那里允许 Bardwave。在其他系统上 Bardwave 不会修改防火墙；如果防火墙已启用，请自行开放该端口（未验证）。
- **租约。** Bardwave 请求两小时的租约，并在租期过半时续租。如果路由器坚持使用永久映射，则每 30 分钟重新检查一次。**Stop Server** 会移除该映射。
- **环回地址或 IPv6 绑定。** 此时会跳过端口转发：路由器只转发 IPv4，而环回套接字无法从外部访问。当绑定地址为特定 IPv4 地址时，映射只指向该地址。
- **测试。** 不支持 NAT 环回的路由器可能会拒绝从内部网络访问该互联网链接。请使用移动数据测试。
- **关闭方法。** 在 `config.toml` 中设置 `upnp = false`，然后重新启动应用。

> **注意：** 如果 UPnP 失败，在路由器上手动转发端口同样可行，但本页不涉及该方法（未验证）。

<a id="player-link-and-access-key"></a>
## 玩家链接与访问密钥

玩家链接的格式如下（启用 HTTPS 时为 `https://`）：

```text
http://<advertised-address>:<port>/?k=<access-key>
```

访问密钥来自 `config.toml` 中的 `[server] access_token`：

- 如果配置中没有密钥，首次启动时会生成并保存一个。
- `access_token = ""`（或仅包含空格）会关闭校验。此时面板会显示 `ACCESS KEY OFF - ANYONE CAN LISTEN`，链接中没有 `?k=`，日志会警告音频流处于开放状态。
- 生成的密钥由 16 个小写字母和数字组成。密钥区分大小写。
- 面板显示 `ACCESS KEY` 以及密钥的前几个字符。旁边的刷新图标会立即签发新密钥，不会弹出确认提示（若密钥为空，它也会重新开启校验）。其提示文字会警告：当前链接将立即失效，所有正在收听的人都会被断开。之后请重新分享新链接。新密钥会保存到 `config.toml`。

该密钥保护音频 WebSocket（`/ws/audio`）和状态 API（`/api/status`）。听众页面及其静态文件（脚本、图标、清单文件）是公开的。

<a id="listener-page"></a>
## 听众页面

玩家在浏览器中打开链接。页面标题为 `Bardwave Listener`。它播放的混音与控制台相同，玩家端无需安装任何软件。

页面打开时先播放 6 到 10 秒的启动动画（当设备请求减少动态效果时约为 0.3 s）。收听不会自动开始：玩家必须点击连接按钮。

> **注意：** 页面从公共 CDN 加载 Tailwind CSS、Iconify 组件和 JetBrains Mono 字体（`cdn.tailwindcss.com`、`code.iconify.design`、`fonts.googleapis.com`）。无法访问互联网的设备可能会显示损坏或未加样式的页面；在离线局域网中的显示效果尚未测试（未验证）。

### 连接

连接按钮会随状态变化：

| 按钮文字 | 情形 |
| --- | --- |
| `Establish Link` | 空闲。点击即可连接。 |
| `Linking...` | 正在连接。按钮不可用。 |
| `Stop Retrying` | 正在等待重新连接。点击即可停止。 |
| `Terminate Link` | 已连接。点击即可断开。 |

状态文字会显示 `Node Offline`、`Linking...`、`Reconnecting #<n>`、`Node Online`、`Access Denied` 或 `Link Error`（当浏览器拒绝启动音频时还会显示 `Audio Error`）。

在打开音频流之前，页面会使用该密钥请求 `/api/status`。如果服务器拒绝该密钥，页面会显示 `Access Denied`，停止重试，并给出说明：密钥更换后显示 `This link is no longer valid: the GM issued a new access key. Ask for the current link.`，对于不带密钥的链接则显示 `This link has no access key. Open the full link from the GM (it ends with ?k=...).`。

### 重新连接

连接中断后，页面会依次以 1 s、2 s、4 s、8 s 的延迟重试，之后延迟为 15 s。每个延迟会随机缩短最多 30%。只有链接保持连接满 10 s 后，退避延迟才会重置。超过 10 s 的握手会被放弃，30 s 无数据的链接会被断开（页面每 10 s 发送一次 ping）。重试次数不限。当选项卡重新可见或浏览器报告网络已恢复时，页面会立即重试，而不再等待延迟结束。当主持人停止服务器时，页面会显示 `Reconnecting #<n>`，直到服务器再次启动。

### 个人音量

**My_Volume**（我的音量）设置本设备自身的音量，范围为 0% 到 100%。它不会改变主持人的混音或其他任何玩家的音量。点击百分比按钮可静音或取消静音；静音时会显示 `MUTED`。滑块采用感知式曲线：增益为百分比的平方。该设置保存在浏览器中（`localStorage`，键为 `bardwave.volume`），因此只对该浏览器生效。由于所有人接收的是同一路混音流，总线（bus）音量无法按玩家单独更改。

### 链路质量

**[ LINK_QUALITY ]**（链路质量）面板显示 **Quality**（质量）、**Latency (est.)**（估计延迟）、**Buffer**（缓冲区）、**Packet loss (10 s)**（10 s 丢包）和 **Screen stay-awake**（屏幕常亮）。音频传输时，Quality 显示 `GOOD`、`FAIR` 或 `WEAK`；否则显示 `IDLE`、`WAITING`、`RECONNECTING` 或 `OFFLINE`。丢包率按最近 10 秒统计，在收到至少 50 个数据包之前显示 `--`。

丢包率为 0.5% 或以上、到达抖动为 30 ms 或以上，或窗口内出现任何缓冲区欠载时，会显示 `FAIR`。`WEAK` 需要持续的问题，而不是单次突发：丢包率为 2% 或以上且分布在至少 3 秒的时间内，抖动为 80 ms 或以上，或窗口内出现 3 次及以上欠载，并且上述情况连续 3 次评估（每秒一次）都成立。经过 5 次正常评估后会清除。随后页面显示：`Weak network: audio may stutter or drop out. Move closer to the router or switch to a better connection.`。页面还会显示实时电平表、缓冲区指示，以及缓冲区欠载、缓冲区溢出和丢帧的计数器。

### 正在播放

当主持人播放场景或音乐曲目时，**Now_Playing**（正在播放）行会显示场景名称，其下显示曲目标题。没有播放内容时该行隐藏。相同的文字也会出现在手机锁屏的媒体控制里。

### 屏幕常亮、锁屏与 PWA

收听时，页面会向浏览器申请屏幕常亮（Wake Lock）。**Screen stay-awake** 会显示：`ON`；`PAUSED`（浏览器已释放该锁）；`BLOCKED`（被拒绝，例如被省电模式阻止）；在普通 `http://` 页面上显示 `N/A (needs https)`；浏览器没有该 API 时显示 `N/A`；未收听时显示 `OFF` 或 `--`。选项卡隐藏时，浏览器会释放该锁；选项卡重新可见时，页面会再次申请。手机和平板的表现尚未测试（未验证）。

页面会注册锁屏媒体控制（Media Session），以场景和曲目作为标题。实时音频流无法原地暂停：在锁屏上暂停或停止会断开链接，再次播放则会重新加入。为使控件出现，页面会播放一段无声的循环音频；它本身不包含任何音频内容。

该页面可以作为 PWA（添加到主屏幕的应用）安装（名称 `Bardwave Listener`，独立显示模式，竖屏）。页面以相同的 `?k=` 密钥请求清单文件，清单文件会把它回显到启动 URL 中，因此主屏幕图标会带着密钥打开。只有由字母、数字、`-` 和 `_` 组成、且不超过 64 个字符的密钥会被回显；手工构造的、含其他字符的密钥会使图标在没有密钥的情况下打开，玩家会收到 `Access Denied`。没有 service worker，因此该页面无法离线工作。安装可能需要安全上下文，参见[HTTPS（安全连接）](./https.md)（未验证）。

<a id="gm-remote-panel"></a>
## 主持人远程面板

主持人面板就是 `/control` 页面（手机上的选项卡标题为 `Bardwave GM`）。它允许主持人在服务器运行期间，通过手机或另一台电脑操作控制台。控制台窗口和无头模式（headless）都会提供该页面。手机发出的命令与在控制台窗口中点击走的是同一条路径，因此两者始终显示相同的状态。

### 主持人令牌

该面板由 `config.toml` 中的 `gm_token` 保护，它与访问密钥是分开的。

- 如果配置中没有令牌，首次启动时会生成并保存一个。其格式与访问密钥相同（由 16 个小写字母和数字组成）。
- `gm_token = ""` 会禁用远程访问，面板只能在本机打开。控制台会显示 `Remote panel is off: it opens on this computer only` 以及 **Create GM key**（创建主持人令牌）按钮，点击后会生成令牌，使面板可以从手机打开。

控制台的 **GM panel link**（主持人面板链接）区块显示不含协议前缀的链接，令牌只显示前四个字符。服务器运行前，它会显示 `Start the server to get the link`。点击链接可复制完整链接。提示文字为 `Do not share with players: this link controls the console`。**Show QR**（显示二维码）会生成该链接的二维码（默认隐藏，因为屏幕可能被玩家看到；服务器停止时该按钮不可用）。**Hide QR**（隐藏二维码）将其移除。**New key**（新令牌）会先请求确认：`The current link stops working at once and every phone using it is disconnected. Players' links are not affected.`。**Replace key**（更换令牌）会签发新令牌并保存。旧链接以及使用它的手机会话会立即失效。**Cancel**（取消）保留当前令牌。

链接格式如下（启用 HTTPS 时为 `https://`）：

```text
http://<advertised-address>:<port>/control?g=<gm-token>
```

### 面板的访问权限

| 请求 | 是否需要令牌 | 结果 |
| --- | --- | --- |
| 来自本机、未经代理、`Host` 为环回地址 | 否 | 允许访问。页面会发送 `X-Bardwave-Control` 请求头。 |
| 来自其他任何位置，或经代理或隧道转发 | 是 | 携带令牌则允许访问，否则返回 `401`。 |
| 使用听众（玩家）密钥 | 不接受 | `401`。 |
| `gm_token = ""`，来自其他任何位置 | 无法访问 | `401`。 |

“本机”由套接字地址决定。不会信任 `X-Forwarded-For`。当请求携带反向代理请求头（`X-Forwarded-*`、`Forwarded`、`X-Real-IP`、`Via`、`CF-Connecting-IP` 及类似请求头）时，或其 `Host` 不是 `localhost`、`127.0.0.1` 或 `[::1]` 时，该请求会被视为远程请求。错误、缺失和已禁用的令牌都会返回相同的 `401`。

令牌可通过 `Authorization: Bearer <token>` 发送，也可通过 `?g=<token>` 发送。手机页面会把令牌从地址中移到 `sessionStorage`（键为 `bardwave.gm.token`），并立即将其从地址栏中移除。在同一选项卡中刷新页面时仍能读取到令牌；新选项卡则需要重新打开完整链接。页面外壳（`/control`、`/control.js`、`/control.css`）对所有人开放，因为其中不包含任何机密。因此，没有令牌时刷新仍会打开页面，随后页面会提示从控制台重新打开链接。

### 手机端可用的功能

携带令牌后，手机页面会提供控制台的以下控件：

- **Master volume**（主音量）、**Duck**（闪避，ducking；处于闪避状态时显示 `Ducked`，并附带 `Ducking: <buses>` 说明）以及 **Stop all**（全部停止）；
- **Scenes**（场景）与 **Stop scene**（停止场景）；
- **Stingers**（短音效，stinger）；
- **Music**（音乐）：播放列表、**Prev**（上一曲目）、**Play**（播放；播放时显示 `Pause`）、**Next**（下一曲目）、**Stop**（停止）、**Shuffle**（随机播放）、**Repeat**（重复；显示为 `Repeat: off`、`all` 或 `one`）、曲目进度条，以及显示当前曲目的一行；
- **Mixer**（混音器）：总线 **Music**、**Ambience**（氛围音）与 **Effects**（音效）；
- **Sounds**（声音）：每个声音都有一个 **Play** 按钮（播放时变为 **Stop**）、一个 **Loop**（循环）开关和一个音量滑块。

状态行会显示 `Connected`、`Connecting…` 或 `Offline, retrying…`。页面会自动重新连接，延迟从 0.5 s 逐渐增长到 10 s。多台设备可以同时连接，所有设备都会看到每一处变化。如果令牌被替换，页面会显示 `The game master link was replaced. Open the new link from the server panel.` 并停止。其他消息包括 `Access denied. Open the game master link from the console again.` 以及 `Open the link from the console`（没有令牌时）。面板会隐藏没有内容可显示的部分（没有场景、没有短音效时）。

### 玩家密钥无法用于控制的原因

访问密钥仅在 `/ws/audio` 和 `/api/status` 上校验。面板路由（`/api/control/*`、`/ws/control`）一律不接受它，无论是放在请求头中、`?k=` 中还是 `?g=` 中，并且主持人令牌也不能当作访问密钥使用。拥有听众链接的玩家可以收听音频流，但无法点击任何控件。只有持有主持人令牌的人，或坐在本机前的人，才能操作。

### 速率限制

- 每个客户端地址在 60 秒内最多允许失败 10 次（IPv6 `/64` 段计为一个客户端）。此后服务器会返回带 `Retry-After` 的 `429`，并且在时间窗口结束之前，即使是正确的令牌也会被拒绝。手机页面随后会显示 `Too many attempts. Wait <n> s.`。来自本机且未经代理的请求不受限制。
- 经本机代理转发的请求共用一个计数器，每分钟允许 60 次失败。同一代理后面的恶意客户端仍可能拖慢其他人。解决办法是轮换主持人令牌，或在本机使用面板。
- `/ws/control` 最多同时接受 16 个客户端（第 17 个会收到 `503`），消息大小为 4096 字节，每个客户端每秒最多 100 条命令（超出的命令会被丢弃，并附带 `too many commands, slow down` 消息）。75 s 无数据的客户端会被关闭（服务器每 25 s 发送一次 ping）。
- `Origin` 不是页面自身主机的本地 WebSocket 会被拒绝（`403`），没有 `X-Bardwave-Control` 请求头的本地 REST 操作也会被拒绝（`403`），因此其他网站的网页无法通过主持人的浏览器操控面板。

> **警告：** 使用普通 HTTP 时，主持人令牌会以明文在网络中传输。在通过互联网使用手机操作面板之前，请启用 [HTTPS（安全连接）](./https.md)。

> **警告：** 普通的 TCP 隧道（`ssh -R`、bore、frp 及类似工具），或将 `Host` 改写为 `127.0.0.1` 的代理，看起来像是本地访问。只有在已设置 `gm_token` 时才使用此类隧道。

<a id="headless-control-page"></a>
## 无头模式下的 /control 页面

使用 `--server-only` 时，应用在没有窗口的情况下运行。它会加载 `config.toml` 中保存的声音库，自行启动听众服务器，并一直运行到按下 Ctrl+C 为止。此时控制页面是唯一的操作界面。在此模式下，快捷键和场次定时器不会运行；在控制台中修改过的声音库需要重新启动服务器才能生效。

```bash
soundshared --server-only
soundshared --server-only --profile <name>
```

`soundshared` 是 `cargo build` 生成的文件名。Linux 的 `.deb` 软件包会将其安装为 `bardwave`。README 中 Windows 发行版使用的是 `Bardwave.exe`（未验证）。`--profile <name>`（或 `--profile=<name>`）用于选择战役的配置方案（profile）。每个配置方案有各自的 `config.toml` 和声音库，存放在数据目录的 `profiles/<name>/` 下。

日志会显示地址。令牌本身不会写入日志：

```text
Listener server available at http://<advertised-address>:<port>/?k=<access-key>
Control page (this PC only): http://127.0.0.1:<port>/control
Remote control: http://<advertised-address>:<port>/control?g=<gm_token from config.toml>
```

只有设置了主持人令牌时才会出现 `Remote control` 行。启用 HTTPS 时协议为 `https`。当路由器授予映射时，日志中会添加一行 `Router forwards ... internet URL ...`。

`GET /control` 对任何人提供页面，因为页面不包含机密。数据路由需要令牌或本地请求。没有令牌的本地请求必须在每次调用时都携带 `X-Bardwave-Control` 请求头（任意值），包括 `GET` 请求；携带令牌的请求则不需要该请求头。

| 路由 | 用途 |
| --- | --- |
| `GET /api/control/state`、`GET /api/control/scenes` | 控制台状态与场景。 |
| `POST /api/control/play`、`stop`（`?id=`）、`stop_all`（`?fade_ms=` 可选）、`volume`（`?id=&value=`）、`loop`（`?id=&on=`）、`master`（`?value=`）、`stinger`（`?id=`）、`duck`（`?mode=` 或 `?on=`）、`bus`（`?bus=music\|ambience\|sfx&value=`） | 声音与混音器命令。 |
| `POST /api/control/scene/activate`（`?id=`）、`scene/stop` | 场景。 |
| `POST /api/control/playlist/<verb>` | 音乐。`<verb>` 为 `start`、`stop`、`pause`、`resume`、`toggle`、`next`、`prev`、`seek`、`shuffle` 或 `repeat`。 |
| `GET /ws/control` | 实时状态。命令以带有 `cmd` 字段的 JSON 帧发送，例如 `play` 或 `volume`。 |

<a id="https"></a>
## HTTPS

当 `tls = true` 时，同一端口会提供 HTTPS 和 `wss://` 而非普通 HTTP，因此访问密钥和主持人令牌在传输过程中会被加密。若未设置 `tls_cert` 和 `tls_key`，会自动生成自签名证书；无法读取的证书会导致启动失败并显示 `Error`，而不会回退到 HTTP。该设置对应面板中的 **Secure connection (HTTPS)**（安全连接）区块，并在下一次 **Start Server** 时生效；[HTTPS（安全连接）](./https.md)介绍了证书设置和浏览器警告。

<a id="configuration-reference"></a>
## 配置参考

所有配置项都位于 `config.toml` 的 `[server]` 表中。

| 配置项 | 类型 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `port` | 整数 | `8080` | 监听端口，范围 `1` 到 `65535`。 |
| `host` | 字符串 | `"0.0.0.0"` | 绑定地址，为 IP 地址。不是 IP 地址的值会回退为 `0.0.0.0`。 |
| `upnp` | 布尔值 | `true` | 请求路由器转发端口。 |
| `access_token` | 字符串 | 首次启动时生成 | 访问密钥。`""` 会将音频流开放给所有能访问该端口的人。 |
| `gm_token` | 字符串 | 首次启动时生成 | 主持人令牌。`""` 仅允许从本机访问。 |
| `tls` | 布尔值 | `false` | 在同一端口上提供 HTTPS 和 `wss://`。 |
| `tls_cert` | 字符串 | 未设置 | PEM 证书链的路径。需要同时设置 `tls_key`。 |
| `tls_key` | 字符串 | 未设置 | `tls_cert` 对应的 PEM 私钥路径。 |

```toml
[server]
port = 8080
host = "0.0.0.0"
upnp = true
access_token = "k3x9q2w7e1r4t6y0"
gm_token = "p8m2n5b1v7c3x9z4"
tls = false
```

使用自有证书时：

```toml
[server]
port = 8443
host = "0.0.0.0"
upnp = true
tls = true
tls_cert = "/etc/ssl/bardwave/fullchain.pem"
tls_key = "/etc/ssl/bardwave/privkey.pem"
```

应用在启动时读取 `config.toml`，因此手动编辑任何配置项后都需要重新启动。此后，服务器停止时可以在面板中修改 `port` 和 `host`（修改会保存），新值将在下一次 **Start Server** 时生效；`tls`、`tls_cert` 和 `tls_key` 则在每次启动时，从正在运行的应用所持有的设置中读取。`upnp` 仅在应用启动时读取。

配置文件保存在数据目录中。如果可执行文件旁边已存在 `config.toml`（在 Windows 上仅当该文件夹可写时），或该处存在 `portable.flag` 文件，或程序所在文件夹可写且不在 macOS `.app` 内，则使用该文件夹。否则，macOS 上为 `~/Library/Application Support/Bardwave`，Windows 上为 `%APPDATA%\Bardwave`，Linux 上为 `$XDG_DATA_HOME/bardwave` 或 `~/.local/share/bardwave`。命名配置方案会在该文件夹内的 `profiles/<name>/` 中保存其 `config.toml`。

<a id="symptoms-and-fixes"></a>
## 现象与解决方法

| 现象 | 可能原因 | 处理方法 |
| --- | --- | --- |
| 听众页面上显示 `Access Denied` | 链接中的密钥过旧或缺失。 | 从 **Copy Link** 复制当前链接。 |
| Listen URL 显示 `127.0.0.1` | 未检测到局域网地址，或绑定地址为环回地址。 | 停止服务器，将 **Bind Address** 设为 `0.0.0.0` 或局域网 IP，然后重新启动。 |
| 局域网中的玩家无法连接 | 防火墙。 | 在 Windows 上允许 Bardwave（也包括公用网络），或以管理员身份运行。在其他系统上，请自行在防火墙中开放该端口（未验证）。 |
| `VPN active (...)` | 默认路由经过 VPN。 | 面向互联网上的玩家时，请关闭 VPN。 |
| `Internet: no UPnP router` | 路由器未启用 UPnP。 | 启用 UPnP，或手动转发端口（未验证）。 |
| `Internet: blocked by a second NAT (...)` | 运营商级 NAT 或额外的调制解调器。 | 向运营商申请公网 IP。 |
| `Remote panel is off...` | `gm_token = ""`。 | 点击 **Create GM key**。 |
| 手机面板上显示 `401` 或 `Access denied` | 主持人令牌缺失或错误。 | 从控制台重新打开主持人面板链接。 |
| 手机面板上显示 `429` 或 `Too many attempts` | 失败尝试次数过多。 | 等待 `Retry-After` 所示的时间。 |
| `The game master link was replaced` | 点击了 **Replace key**。 | 从控制台打开新的主持人链接。 |
| `Weak network: ...` 提示 | 听众连接存在丢包或抖动。 | 靠近路由器，或更换网络连接。 |
| Stay-awake 显示 `N/A (needs https)` | 普通 `http://` 页面。 | 启用 [HTTPS（安全连接）](./https.md)。 |

<a id="not-verified"></a>
## 未验证

以下各项来自代码与测试，但未在真实硬件或真实网络上检验：

- 真实机器上的 Windows 防火墙规则，以及其他系统对各自防火墙的处理方式。
- 真实路由器上的 UPnP，包括双重 NAT 的情况以及只接受永久映射的路由器。
- 听众页面在 iOS Safari 和 Android Chrome 上的表现：屏幕常亮、锁屏控件、主屏幕安装，以及页面在无互联网接入的局域网中的显示效果。
- 用手机摄像头扫描二维码。
- 发行版构建的 Windows 可执行文件名称。
