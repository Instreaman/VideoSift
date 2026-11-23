# 来源适配整合与整体验证

Rust core 继续使用共享 `VideoSource` 契约。本地文件的标识来自内容摘要，`part_id` 为 `null`。
Bilibili 的标识为 BV，`part_id` 为从 1 开始的分 P 序号。网络请求使用该分 P 在目录中的 `cid`。

## 调用流程

1. 本地来源通过 `validate_local_file` 校验调用方在授权根下选定的普通文件，成功后使用
   `VideoSource::Local`。文件级检查覆盖路径授权、非空和内容摘要。媒体格式与可播放性留待媒体处理。
   [本地文件与平台限制](LOCAL_INPUT.md)
2. Bilibili 来源通过 `fetch_metadata_with` 复用离线输入解析并取得经过校验的分 P 目录。
   多 P 且未明确选择时返回 `SelectionRequired`。调用方可用同一目录的 `VideoMetadata::select` 继续选择。
   只有 `Selected` 提供可用于后续请求的 `BilibiliSource`。
   [离线输入](BILIBILI_INPUT.md)与[元数据](BILIBILI_METADATA.md)
3. 将选定分 P 和目录交给 `bilibili::media::discover_page_media_with`。有字幕时返回
   `PageMedia::Subtitles`，由调用方从目录中选择一条，交给 `fetch_subtitle_with` 获取并校验时间轴。
   只有成功且显式返回空字幕列表才解析同一分 P 的音轨，返回 `PageMedia::Audio`，供 `fetch_audio_with`
   获取有界正文并检查顶层 MP4 结构。目录异常、权限、限流和超时会终止发现。字幕获取失败沿用字幕错误。
   [字幕发现与获取](BILIBILI_SUBTITLES.md)与[音轨回退](BILIBILI_AUDIO.md)

协调入口需要可信调用方注入 GET 传输。它在发现字幕后按需请求音轨清单，最多发起两次顺序请求。
真实 HTTPS 入口仍按各操作分开调用，调用方须先获得授权，并控制请求节奏、遵循 `Retry-After`。
本次整合范围为 Rust core 库入口，独立 CLI 继续提供合成任务演练。

## 验收覆盖

| 场景 | 合成用例核对内容 |
| --- | --- |
| 本地文件 | 授权根内文件、根外路径拒绝、与共享本地样例一致的序列化 |
| Bilibili 分 P | 裸 BV 需要选择、URL 指定 P2、复用目录、共享 Bilibili 样例、P2 的 `cid` |
| 字幕优先 | 一次目录发现，随后获取明确选择的字幕，按 P2 时长校验 |
| 无字幕回退 | 仅显式空目录才请求清单，再获取同一分 P 的音轨正文 |
| 错误边界 | 超长输入在发出请求前拒绝。目录异常、403、429、超时或无效字幕均无法触发回退。无效清单在下载前失败 |

验证使用本地临时合成文件和内存 Mock 响应。真实平台兼容性、音频可解码性与性能仍待验证。
