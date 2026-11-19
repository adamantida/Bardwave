# HTTPS（安全连接）

Bardwave 可以通过 HTTPS 和 `wss://` 而非普通的 HTTP 和 `ws://` 提供页面与音频流。该功能**默认关闭**。本页说明它保护什么、如何开启，以及浏览器发出警告时如何处理。

该设置位于控制台 NETWORK 列中的 **SECURE CONNECTION (HTTPS)**（安全连接）部分（该面板标题为 *Network Server*（网络服务器），下文称为 SERVER panel（服务器面板）），也可在 `config.toml` 中通过 `[server] tls`、`tls_cert` 和 `tls_key` 设置。俄文版本：[../ru/https.md](../ru/https.md)。

<a id="why-https"></a>
## HTTPS 的作用

玩家的访问密钥（`?k=...`）和主持人（GM）的令牌（`?g=...`）随链接一起传输，并且每个密钥还会在其 WebSocket 握手的查询参数中再次发送（玩家使用 `/ws/audio`，主持人使用 `/ws/control`）。在普通 HTTP 下，它们以明文在网络中传输。在互联网上，传输路径上的任何人（例如共享 Wi-Fi、网络运营商或隧道主机）都可以读取这些凭据，进而收听音频流，或按下主持人面板上的按钮。

使用 HTTPS 时，同一端口改为通过 TLS 通信，因此凭据和音频在传输过程中都会加密。

还有第二个原因。浏览器只向*安全上下文*开放部分功能：**Wake Lock**（收听时保持手机屏幕常亮）、将页面安装为 **PWA**（添加到主屏幕的应用）以及 **AudioWorklet**。在局域网（LAN）地址上通过普通 HTTP 访问时，听众页面仍可播放，因为它会回退到较旧的音频路径（ScriptProcessor）；但开始收听后，其 **Screen stay-awake**（屏幕常亮）行会显示 `N/A (needs https)`（收听开始前显示 `--`）。使用 HTTPS 时，页面处于安全上下文，可以使用以上全部三项功能（主屏幕安装本身未验证）。

说句实话：**在家庭网络中通过普通 HTTP 使用时，一切都正常。** HTTPS 针对的是互联网场景和上述额外功能。

<a id="when-you-do-not-need-it"></a>
## 无需使用 HTTPS 的情况

- 所有人都位于同一房间或同一家庭网络中（即 `http://192.168.x.x:8080` 链接）。外部人员无法看到这些流量。
- 通过本身已加密连接的 VPN 或可信隧道联系玩家。
- 仅在本机上使用控制台。

此时应保持 HTTPS 关闭。开启后只会因自签名证书多出一条证书警告，不会带来任何需要的功能。

<a id="quick-start"></a>
## 快速开始

1. 在 SERVER panel 中向下滚动到 **SECURE CONNECTION (HTTPS)**，然后点击 **On**（开启）。该部分的其余内容随即展开。保持选中 **Self-signed (automatic)**（自签名，自动）。
2. 点击 **Start Server**（启动服务器）；若服务器已在运行，则先点击 **Stop Server**（停止服务器），再点击 **Start Server**（此时该部分会显示 *Restart the server to apply*）。
3. 从面板中复制玩家链接或主持人链接。此时链接以 `https://` 开头。首次访问时，每位玩家的浏览器都会显示证书警告：应点击哪个按钮，参见 [自签名证书](#self-signed-certificate)；同时请比对该部分显示的指纹（fingerprint）。

该设置保存在 `config.toml` 中，并在下次启动服务器时生效。服务器重新启动之前，不会有任何加密。

<a id="self-signed-certificate"></a>
## 自签名证书

选择 **Self-signed (automatic)** 后，Bardwave 会自行生成证书（仅生成一次），并保存在数据目录的 `tls/` 文件夹中（`cert.pem`、`key.pem` 和 `meta.json`）。当程序自身所在文件夹可写时（便携模式、`portable.flag`，或该文件夹中已有 `config.toml`），数据目录即为程序所在文件夹；否则，macOS 上为 `~/Library/Application Support/Bardwave`，Windows 上为 `%APPDATA%\Bardwave`，Linux 上为 `$XDG_DATA_HOME/bardwave` 或 `~/.local/share/bardwave`（参见 README 中的 *Where the data lives*）。命名的配置方案（profile）各有自己的 `profiles/<name>/tls/`。

该部分根据已保存的内容显示以下信息：

- 证书的 **SHA-256 指纹**，附带 **Copy**（复制）按钮；
- 其 **expiry date**（有效期）和剩余天数。证书的有效期为 397 天。下次启动服务器时，若剩余不足 30 天（或其起始日期在未来），Bardwave 会替换该证书；
- **Issued for**（签发对象）：证书涵盖的名称和地址。包括 `localhost`、`127.0.0.1`、`::1`、本机的主机名（主机名中不含点号时还包括 `<host>.local`；Windows 上的主机名读取自 `COMPUTERNAME`，此项未验证），以及生成证书时本机的局域网地址。若计算机之后连接到其他网络，且其当前的局域网地址均未被涵盖，则下次启动服务器时会生成新证书；
- **Regenerate certificate**（重新生成证书）：会先询问，点击 **Regenerate**（重新生成）确认，或点击 **Cancel**（取消）。它会立即用新证书替换旧证书，但正在运行的服务器会继续使用旧证书，直至重新启动。之前信任旧证书的每位玩家都会再次看到警告。该按钮仅在 **Self-signed (automatic)** 模式下才有。

首次启动服务器前，该部分显示 *A certificate is made on the next Start*。选择 **My certificate**（自有证书）但未填写任何文件时，在提供两个文件之前仍会使用自动生成的证书（该部分对此有说明）。

### 玩家看到的内容

自签名证书不是由浏览器认可的机构签发的，因此首次访问时会显示警告。这是正常现象。不同浏览器和版本的措辞各不相同；以下为常见的操作路径（未验证：本项目尚未在真实手机或当前版本的浏览器上核对这些界面，参见 README 中的 *Not verified on real hardware* 列表，即“真实硬件上未验证”）：

| 浏览器 | 显示内容 | 如何继续 |
|---|---|---|
| Chrome（桌面版和 Android） | “Your connection is not private”，`NET::ERR_CERT_AUTHORITY_INVALID` | 依次点击 **Advanced**、**Proceed to ... (unsafe)** |
| Safari（macOS） | “This Connection Is Not Private” | 依次点击 **Show Details**、**visit this website** |
| Safari（iOS） | “This Connection Is Not Private” | 依次点击 **Show Details**、**visit this website**，然后确认 |
| Firefox（桌面版和 Android） | “Warning: Potential Security Risk Ahead” | 依次点击 **Advanced...**、**Accept the Risk and Continue** |

另有两种情况会让警告多出内容。若证书未涵盖玩家输入的地址（例如公网 IP 或域名，生成的证书不会列出这些地址），则在颁发机构警告之外，还会显示一条名称不匹配的消息；按钮相同，继续操作即可。此外，证书重新生成或替换后，浏览器会再次询问。

### 指纹比对

只有当证书确实是自己的，才值得点击跳过警告。在警告页面上打开证书详情（Chrome：点击锁形图标或 **Not secure** > certificate；Firefox：**Advanced...** > **View Certificate**；Safari：**Show Details** > **view the certificate**；菜单名称未验证），然后将其 **SHA-256** 指纹与 SERVER panel 中显示的指纹（或日志行 `HTTPS enabled ... SHA-256 ...` 中的指纹）进行比对。面板以冒号分隔的大写十六进制字节对形式显示指纹。部分手机浏览器不显示指纹；此时请在电脑上核对，或把用 Copy 复制的文本通过可信渠道发给玩家。

<a id="using-your-own-certificate"></a>
## 自有证书的使用

由真实的证书颁发机构（CA，例如 **Let's Encrypt**）签发的证书会被所有浏览器信任，因此玩家不会看到警告。使用这种证书需要一个指向本机或路由器的域名：证书是为该域名签发的，玩家必须用该域名打开链接。

1. 以 **PEM** 文件形式获取证书及其私钥。Caddy、certbot 或 acme.sh 等工具会以文件形式保存它们；请使用完整证书链文件（例如 `fullchain.pem`）作为证书。私钥可以是 RSA、ECDSA 或 Ed25519（即 rustls 搭配 `ring` 提供者能够签名的类型）。不支持带口令保护的私钥（未验证）。
2. 在该部分选择 **My certificate**。将两个路径分别填入 **Certificate file (PEM)**（证书文件）和 **Private key file (PEM)**（私钥文件），或点击各自旁边的 **Browse...**（浏览）。
3. Bardwave 会在后台检查文件，并在出问题的输入框下方说明原因（找不到文件、不是证书、密钥与证书不匹配）。两者均无误后，将显示指纹。
4. 依次点击 **Stop Server** 和 **Start Server**（若服务器已停止，则直接点击 **Start Server**）。路径会保存为 `tls_cert` 和 `tls_key`：

```toml
[server]
tls = true
tls_cert = "/etc/ssl/bardwave/fullchain.pem"
tls_key  = "/etc/ssl/bardwave/privkey.pem"
```

说明：

- 需要同时设置两个路径。只设置其中一个时，服务器**不会以 HTTPS 启动**（它绝不会悄悄回退到自签名证书或普通 HTTP）。该部分会对此给出警告。空路径或仅含空白字符的路径视为未设置。若切换回 **Self-signed (automatic)**，两个路径会从 `config.toml` 中删除。
- **续期：** Bardwave 仅在服务器启动时读取证书文件。证书工具续期后，请依次点击 **Stop Server** 和 **Start Server**。该部分每隔几秒检查一次文件；当文件的指纹与正在运行的证书不同时，会显示 *Restart the server to apply*。Bardwave 不会读取自有证书的有效期，因此需自行记得续期。
- **位于 Caddy 或其他反向代理之后：** 更简单的做法是由代理持有证书，并在同一台机器上通过普通 HTTP 与 Bardwave 通信，同时关闭 Bardwave 的 HTTPS。代理必须转发 WebSocket（`/ws/audio` 和 `/ws/control`）。在此配置下，控制台显示的链接以 `http://` 开头；请改为分享代理的 `https://` 地址。务必保持 `gm_token` 为已设置状态：Bardwave 根据套接字地址判断是否为本机，携带代理请求头或 `Host` 不是环回地址的请求会被视为远程请求，需要令牌；而不添加此类标记的普通 TCP 隧道，在 Bardwave 看来与本机浏览器无异（参见 README 中的 *Security and tokens*）。另一种做法是按上文所述，将代理的证书文件交给 Bardwave。

<a id="what-changes"></a>
## 变化内容

开启 HTTPS 并（重新）启动服务器后：

- **LISTEN URL**（监听地址）、**Copy Link**（复制链接）、**LOCAL**（本机）地址，以及来自路由器映射的 **INTERNET**（互联网）地址（UPnP，已开启时）均以 `https://` 开头。
- **GM panel link**（主持人面板链接）及其二维码（**Show QR**（显示二维码））均由同一地址生成，因此同样以 `https://` 开头。
- 听众页面和主持人面板在同一页面地址和端口上，以 `wss://` 打开音频流和控制套接字。没有第二个端口，也没有重定向。
- **同一端口上的普通 `http://` 请求不会得到响应。** 输入 `http://` 的玩家将无法打开页面。之前以 `http://` 分享的链接将失效；请分享新链接。
- 使用 `--server-only` 的无头模式（headless）时，`server.tls` 同样生效，日志会显示证书指纹。若无法加载证书，日志会显示 `Failed to start listener server`，且不会启动服务器。

其余均不变：凭据、声音库和音频都和之前一样。

<a id="turning-it-off"></a>
## 关闭方法

在该部分点击 **Off**（关闭）或设置 `tls = false`，然后依次点击 **Stop Server** 和 **Start Server**。链接会恢复为 `http://`。该部分随后再次只显示一行。

设置会保留：生成的证书仍在 `tls/` 文件夹中，路径仍保留在 `config.toml` 中；再次点击 **On** 即可复用。如需清除，请自行删除 `tls/` 文件夹。

<a id="troubleshooting"></a>
## 故障排查

| 现象 | 原因 | 处理方法 |
|---|---|---|
| 启动时服务器面板显示 **Error**（错误），按钮变为 **Retry Server**（重试服务器），且服务器无法启动 | 证书或密钥文件无法读取、内容不是证书，或密钥与证书不匹配；也可能只设置了两个路径中的一个。 | 打开该部分，阅读输入框下方的消息，修正路径或文件。状态行只显示消息的开头部分，完整内容见日志。也可以改选 **Self-signed (automatic)**。 |
| 该部分显示 *Restart the server to apply* | 服务器运行期间设置或证书发生了变化。 | 依次点击 **Stop Server** 和 **Start Server**。 |
| 手机显示证书警告 | 证书为自签名证书。 | 比对指纹，然后按上表所述继续操作；也可以改用自有证书。 |
| 警告提及名称或地址 | 证书未列出链接中的地址（公网 IP、域名，或生成证书时尚不存在的局域网地址）。 | 计算机切换到其他网络后，下次启动时会获得新证书（或点击 **Regenerate certificate**）。对于域名，请为该域名使用自有证书。 |
| 接受后警告再次出现 | 证书已重新生成、替换或续期，或浏览器忘记了例外设置。 | 比对新指纹，然后再次继续。 |
| `http://` 链接打不开或报错 | HTTPS 已开启：该端口仅响应 TLS。 | 使用面板中的 `https://` 链接。 |
| HTTPS 已开启，但链接仍以 `http://` 开头 | 切换后未重新启动服务器。 | 依次点击 **Stop Server** 和 **Start Server**。 |
| Chrome 提示证书已过期 / 尚未生效 | 玩家设备的时钟不正确，或证书较旧。 | 校正时钟。生成的证书生效时间提前一天，以容许时钟的小误差。已过期的自动生成证书会在下次启动时替换（或点击 **Regenerate certificate**）。 |
| 互联网上的玩家无法连接 | HTTPS 不会开放端口。 | 参见 README 中的 *Players outside your network*（路由器映射、防火墙）。 |

<a id="limitations-and-security-notes"></a>
## 局限性与安全说明

- HTTPS 保护的是链接**在传输途中**的安全。它无法保护已经**落入不该拿到的人手中**的链接。访问密钥和主持人令牌仍然包含在 URL 中；任何获得链接的人（聊天消息、截图、共享屏幕、被摄像头拍到的二维码）都能获得访问权限。请将二者都视为机密，切勿把主持人链接或其二维码交给玩家。*Access key*（访问密钥）旁边的 ⟳ 按钮会更换访问密钥，GM panel link 旁边的 **New key**（新令牌）会更换主持人令牌。
- 只有比对过指纹，自签名证书才起到验证作用。不做这项核对，点击跳过警告的玩家就可能是在和别人通信。
- 生成的私钥保存在 `tls/key.pem` 中；在 macOS 和 Linux 上，文件权限为 `0600`，文件夹权限为 `0700`。在 Windows 上，Bardwave 不对其额外设置限制，因此依赖数据目录自身的访问权限。对于自有私钥，请自行限制文件权限（例如在 Linux 和 macOS 上使用 `chmod 600`）。
- `config.toml` 以明文保存凭据，以及证书和私钥的路径。也请妥善保护该文件。
- Bardwave 通过 TLS 提供 HTTP/1.1 服务，使用其 TLS 库（rustls）的标准安全协议版本。不支持客户端证书登录，也不能为生成的证书添加其他名称。
- HTTPS 仅保护浏览器与 Bardwave 之间的流量。它不会隐藏有人正在连接的事实，也不会改变谁能访问该端口。
- 使用真实 Let's Encrypt 证书链时的行为，以及真实 iPhone 和 Android 手机上的警告界面，已有自动化测试和代码审查覆盖，但尚未在真实设备上试过（参见 README 中的 *Not verified on real hardware*，即“真实硬件上未验证”）。
