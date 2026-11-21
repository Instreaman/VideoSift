# Bilibili 离线输入与分 P 校验

Rust core 提供 `bilibili::BilibiliInput::parse` 和 `select`。调用方先解析输入，可用
`source_id()` 取得规范 BV，再提供该 BV 的完整分 P 目录。整个过程只处理内存数据。

## 输入规则

- 输入上限 2048 个 UTF-8 字节，包括 URL 的全部字符。采用精确匹配，保留 BV 大小写。
- 接受独立 BV，形状为大写 BV 加 10 个 ASCII 字母或数字，与公共 Schema 一致。
- 播放页前缀严格为 `https://www.bilibili.com/video/`，之后是一个 BV、可选的一个末尾斜杠、
  可选的 `?p=N`。带/不带末尾斜杠得到同一来源标识。
- P 是 1 至 4294967295 的 ASCII 十进制整数，首位为 1 至 9。裸 BV 只能携带调用方另传的选择。
  空白、百分号编码、显式端口、用户信息、片段、空查询、重复或额外参数均拒绝。
- URL 和调用方同时指定 P 时须一致。零值、冲突即使目录缺失也会返回错误。

## 分 P 结果

调用方以 `PageCatalog` 提供来源 BV 和按播放顺序排列的完整序号（1 至 N）。
空目录、缺号、乱序、重复号或目录属于另一个 BV 均返回有类型的错误。
调用方负责目录的完整性；仅凭连续序号无法判断末尾是否缺页。

| 输入情况 | `select` 返回 |
| --- | --- |
| 未提供目录 | `MetadataRequired`，保留已请求的 P 或空值。实际视频和分 P 存在性待核验。 |
| 单 P，未指定 | `Selected`，序号为 1。 |
| 多 P，未指定 | `SelectionRequired`，提供可选数量，等待显式选择。 |
| 显式指定目录内的 P | `Selected`，其序列化字段为 kind=bilibili、source_id、part_id。 |
| 显式指定的 P 越界 | `PageOutOfRange`。 |

`Selected` 表示通过调用方目录校验。仅此结果可用作公共 VideoSource；其他结果供 Rust 调用方处理。
格式、参数、冲突和越界错误适用 Validation 类别，目录身份和内容错误适用 Source 类别。
本次提供 core 库入口。CLI、公共 RPC、SQLite 和网络适配器各自沿用已有边界。

## 开源参考核对

已验收架构为后续网络与下载指定 Reqwest 和受监管的 yt-dlp。此前决策和依赖记录中没有指定 BiliSum 仓库或移植方式。
本次选取与项目描述相符的 lycohana/BiliSum 作为行为对照，核对提交 `2ce5409a3a843729781cde203d3627cad77a1a17`。

- Python 的 normalize_video_url 在原文中搜索 BV，再构造播放页地址；extract_bilibili_page 从查询中提取 P。
  其测试也覆盖裸 BV、带 P 的裸 BV 和播放页。见[解析实现](https://github.com/lycohana/BiliSum/blob/2ce5409a3a843729781cde203d3627cad77a1a17/packages/core/src/video_sum_core/utils.py)和[分 P 测试](https://github.com/lycohana/BiliSum/blob/2ce5409a3a843729781cde203d3627cad77a1a17/tests/unit/test_bilibili_multi_page.py)。
- resolve_video_page 在已有目录内查找序号，缺省序号为 1。VideoSift 借鉴将来源标识与页选择分开的做法，
  并按已验收契约限制整个输入、多 P 须显式选择。见[分 P 查找](https://github.com/lycohana/BiliSum/blob/2ce5409a3a843729781cde203d3627cad77a1a17/apps/service/src/video_sum_service/video_assets.py#L610)。
- 上游采用 MIT，版权归属为 2026 lycohana。复制软件或其实质性部分须保留版权与许可通知。
  在 Rust core 独立实现此有限文法和目录校验，避免引入 Python 服务和网络链路。此次没有复制代码或测试。见[上游许可证](https://github.com/lycohana/BiliSum/blob/2ce5409a3a843729781cde203d3627cad77a1a17/LICENSE)。

验证使用合成目录与共享 [VideoSource 样例](../contracts/examples/valid.json)。短链、重定向、真实元数据、
字幕、音轨与下载留待相应子任务。此处没有平台可用性或性能测量结论。
