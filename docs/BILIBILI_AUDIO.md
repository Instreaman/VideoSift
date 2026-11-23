# Bilibili 无字幕时的音轨回退

## 触发条件

调用方先使用已校验的分 P 完成 `discover_subtitle_availability_with`。
请求成功且字幕目录显式返回空数组时，才会得到 `SubtitleAvailability::Absent` 和该分 P 的
`ConfirmedNoSubtitles`。Rust core 仅在发现入口内部构造该值，回退请求复用它所记录的 BV 和 `cid`。
有字幕时返回轨道目录。下载字幕失败、目录结构异常、`cid` 不匹配、权限、风控及限流错误都沿用字幕错误，
这些情况均无法取得回退所需的值。原有字幕发现入口继续返回轨道列表。

## 获取流程

1. `resolve_audio_with` 使用确认无字幕的分 P，向固定 HTTPS 端点
   `https://api.bilibili.com/x/player/playurl` 发送 BV、`cid` 和 `fnval=16`。
   响应若提供 `cid`，必须与请求一致。明确标记为受保护时返回 `ProtectedMedia`。
2. 仅查看 `dash.audio`，最多接受 32 个条目，选取第一个声明为 `audio/mp4` 和
   `mp4a.40.2`（AAC-LC）的条目。无支持条目返回 `NoSupportedAudio`，缺失或异常的清单返回错误。
   视频流、其他编解码器和备用下载地址均不会触发下载。
3. `fetch_audio_with` 获取该条目的原始字节，校验 MP4 首个 `ftyp` 或 `styp` box 的类型和大小。
   这项校验可以排除空正文、普通 JSON 等明显不符的响应，完整容器和可解码性由后续媒体处理验证。

## 网络和资源限制

对应的真实 HTTPS 入口为 `discover_authorized_subtitle_availability`、`resolve_authorized_audio`
和 `fetch_authorized_audio`，均须调用方事先取得单独的真实访问授权。
这些调用分开执行，调用方在阶段间遵守共享的单进程请求间隔及返回的 `Retry-After`，
可保留已发现的无字幕结果或音轨句柄，避免重复请求上一步。
真实传输通过 Reqwest 获取这条受限的 DASH 音轨，采用现有的公网 IPv4 固定与限流策略。
关闭代理、重定向和自动重试，沿用权限和风控错误，不加载 Cookie。
目前使用直接 HTTPS 获取单条音轨，不启动下载子进程。

下载只接受 HTTPS 或可补全为 HTTPS 的相对协议地址，主机限
`upos-sz-mirrorcos.bilivideo.com` 和 `upos-sz-mirrorali.bilivideo.com`，
路径限 `/upgcxcode/` 下的 `.m4s` 文件。校验后的下载地址保存在不公开的轨道字段内，
保留签名查询参数，错误及调试输出均省略地址。清单限 1 MiB，
音轨正文限 32 MiB，成功后返回有界内存缓冲区。每次请求的网络时限为 10 秒。

Mock 和合成响应验证 P2 的 `cid`、字幕优先、明确无字幕时下载单条音轨、禁止目的地、受保护内容、
无支持条目、异常正文、限流、超时和字节上限。真实接口、字段、CDN 覆盖、签名、DNS/TLS、可解码性与性能均待验证。
