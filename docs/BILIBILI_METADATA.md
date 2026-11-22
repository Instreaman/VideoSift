# Bilibili 元数据获取与分 P 校验

Rust core 的 `bilibili::metadata` 接受独立 BV 或标准播放页 URL，以及可选目标 P。两个入口复用[离线解析与分 P 选择](BILIBILI_INPUT.md)。`fetch_metadata_with` 接受调用方提供的 GET 传输，测试在内存中返回合成 HTTP 响应。`fetch_authorized_metadata` 执行 HTTPS 请求，调用方须事先取得真实访问授权。CLI 没有调用该入口。

## 请求与响应

1. 解析输入并检查 URL 与调用方的 P 是否冲突。成功后仅向固定 HTTPS 主机 `api.bilibili.com` 的 `/x/web-interface/view` 请求规范化的 `bvid`。分 P 在返回的目录中选择。
2. HTTP 200 且业务 `code` 为 0 时读取 `data`。要求 `bvid` 与请求一致，`videos` 在 1 至 1000 内且与 `pages` 长度相同。页号须按顺序从 1 连续编号，`cid` 为非零且互异的无符号整数。视频标题及分 P 标题须包含非空白字符，各限 1024 个 Unicode 标量。每页 `duration` 接受非负 32 位整数秒，对调用方提供毫秒。
3. 返回视频标题、目录与选择结果。单 P 可直接选中，多 P 缺少目标时返回 `SelectionRequired`。调用方展示目录后可用 `VideoMetadata::select` 显式选择，无须再次请求。选中结果沿用共享 VideoSource 的 BV 和从 1 开始的 `part_id`；目录中的 `cid` 单独保留。标题只作为不可信文本。
4. 输入冲突、选择越界、来源 BV 不匹配或目录页号未从 1 连续排列时，沿用 `SourceInputError`。获取失败不产生可用来源。只认本次响应中的目录，平台后续编辑可能改变分 P 顺序与时长。

对 HTTP 200 响应，先检查正文字节预算，再解析元数据：

- `TooLarge`：已知正文长度超过 1 MiB（1,048,576 字节），或分块读取时累计长度超过该值。恰好 1 MiB 的正文仍进入解析。
- `InvalidMetadata`：JSON 无法解析，或响应缺少有效的整数 `code`；当 `code` 为 0 时，`data` 缺失、为 `null`、缺少必填字段，或字段类型、数值无法按要求解析（如负时长），均返回此错误。
- 以下目录内容也统一返回 `InvalidMetadata`：`videos` 超出 1 至 1000 或与实际页数不符；视频或分 P 标题为空、全为空白或超过 1024 个 Unicode 标量；`cid` 为零或重复。

## 网络与资源限制

- 使用既定的 Reqwest（0.12.28，MIT 或 Apache-2.0），通过 rustls 校验证书。关闭代理、重定向及自动重试，无 Cookie 或登录配置。只连接经校验并固定的公网 IPv4 地址；解析结果含受限 IPv4 或没有可用 IPv4 时返回目的地错误。IPv6-only 环境暂不支持。
- 实际请求在单进程内最多每秒启动一次。每次操作最多一个 GET，网络总时限 10 秒（包括 DNS 和读完整响应），响应按块累计，上限 1 MiB。取消等待可直接丢弃 future。
- HTTP 429 传回 `RateLimited` 和可解析的 `Retry-After`（秒数或 HTTP 日期，使用 httpdate 1.x，MIT 或 Apache-2.0）。调用方若另行安排重试须遵守这个等待时间。认证、禁止访问、风控、找不到视频及其他来源错误以固定错误返回，不输出平台消息、响应正文或重定向位置。
- 验证只用合成数据和虚拟时钟，覆盖目录与选择、结构变化、HTTP/业务错误、限流、超时、流式字节上限及目的地过滤。平台端点、字段实际兼容性、TLS 连通性和性能均待实测。此子任务仅涉及元数据。
