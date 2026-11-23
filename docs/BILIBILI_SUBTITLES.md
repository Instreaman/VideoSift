# Bilibili 字幕发现与获取

Rust core 通过 `bilibili::subtitles` 提供轨道发现和正文获取。这两个操作各有可注入传输的
`discover_subtitles_with`、`fetch_subtitle_with`，供 Mock 测试使用。对应的
`discover_authorized_subtitles`、`fetch_authorized_subtitle` 须由调用方先取得真实访问授权。
当前 CLI 未接入，音轨回退留待后续授权。

## 从分 P 到字幕轨道

1. 传入[已校验的元数据](BILIBILI_METADATA.md)和选中的 `BilibiliSource`。核对 BV 与
   `part_id`，从目录读取相应 `cid`，然后以 BV 和 `cid` 请求
   `https://api.bilibili.com/x/player/v2`。响应可省略 `data.cid`。显式 `null` 或无法解析为无符号整数的值
   返回 `RequestError::InvalidResponse`，与请求不匹配的无符号整数返回 `InvalidCatalog`。
2. 业务响应须有 `code: 0` 和完整的 `data.subtitle.subtitles` 数组。仅显式空数组表示无字幕。
   数组缺失、为 null 或格式不符均返回错误。目录最多 32 条轨道，每条须有 2 至 35 字节的 ASCII
   语言标识（字母数字段，可用连字符分隔）、非空白且不超过 128 个 Unicode 标量的名称，以及合规地址。
3. 调用方从目录选择轨道。获取操作直接使用该轨道内的地址，保留签名查询参数。
   轨道不公开下载地址，调试输出也省略该信息。

## 正文与错误

- 正文接受 JSON `body` 数组，每条含 `from`、`to` 和 `content`。时间以秒为输入，
  转为最接近的整数毫秒，端点范围沿用共享契约。起点非负且按顺序不减，末端须晚于起点，允许重叠。
- 字幕结束时间的上限来自所选分 P 的元数据。元数据时长只精确到整数秒，按该值作为严格上限，
  恰好等于上限有效。结束时间在转换为毫秒前就会比较，超过上限即返回 `InvalidSubtitle`，
  包括会舍入到同一毫秒的超出量。若平台时长省略了末尾的小数秒，该段字幕可能被拒绝。
  元数据为 0 秒时，任何非空时间区间均无法通过。
- 正文须有 1 至 10000 条，单条文本须包含非空白字符，最多 65536 个 Unicode 标量。
  缺字段、空正文或无效条目返回 `InvalidSubtitle`。字幕文本原样作为不可信数据返回，
  调用方在构建任务 `Transcript` 时再分配证据标识。
- 分 P 与目录不匹配返回 `InvalidPage`。发现时无效目录内容返回 `InvalidCatalog`，
  响应结构错误返回 `RequestError::InvalidResponse`。权限、风控、找不到资源、超时、
  字节超限和 HTTP 429 均有独立错误，429 携带可解析的 `Retry-After`。
  错误消息省略响应正文、平台消息、下载地址和签名。

## 网络边界与验证

- 下载仅接受 HTTPS 或可补全为 HTTPS 的协议相对地址。允许
  `aisubtitle.hdslb.com/bfs/ai_subtitle/` 和 `i0.hdslb.com/bfs/subtitle/` 下的文件。
  路径段限定为 ASCII 字母、数字、点、下划线或连字符。带用户信息、显式端口、片段或其他目的地的地址均拒绝。
- 与元数据共用真实传输：校验并固定公网 IPv4，关闭代理、重定向和自动重试，单进程请求启动的间隔至少一秒。
  发现和获取各有 10 秒的网络时限，正文分别限 1 MiB 和 2 MiB。
- Mock 使用合成目录与字幕正文，验证指定分 P、显式无字幕、异常结构、权限和限流、禁止目的地、
  超时、大小限制和时间范围。端点、字段、CDN 白名单覆盖、真实 DNS/TLS 连通性与性能仍待另行授权后验证。
