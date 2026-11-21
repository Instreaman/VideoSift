# 统一工程风格

## 原则

清晰、简洁、类型安全、职责单一、依赖最小化、错误可追踪。禁重复工具层、无用依赖、
死代码、无意义抽象、占位功能、无关重构、大规模 suppress 和未经验证的性能宣传。
函数以能单独描述、测试的一项行为为边界；复杂时按职责拆分，不按任意行数机械拆分。

所有文本 UTF-8 无 BOM、LF、末尾换行；批处理脚本可 CRLF，由 .gitattributes 管理。
缩进见 .editorconfig；Markdown/JSON 两空格，代码四空格，Rust 按 rustfmt。
源码注释/API 文档用简洁英文，解释约束、原理和原因；用户文档简体中文，README 双语。

## 共享领域词汇

| 概念 | 统一标识 | 语义 |
| --- | --- | --- |
| 视频 | Video | 可独立处理的某一分 P 内容 |
| 视频来源 | VideoSource | local 或 bilibili 及稳定来源标识 |
| 媒体流 | MediaStream | 视频/音频轨道及时间基 |
| 处理任务 | ProcessingTask | 一个可恢复的流水线执行实例，禁止别称 Job/Work/Mission |
| 流水线 | Pipeline | 有界步骤及依赖关系 |
| 文本 | Transcript / TranscriptSegment | 原始或识别文本及媒体相对时间范围 |
| 关键帧 | Keyframe | 媒体时间戳及文件引用 |
| 知识 | KnowledgeItem | 有来源类别、证据关联和不确定性的断言 |
| 摘要 | Summary | 结构化知识输出，不代表视频原话 |
| 推理后端 | InferenceBackend | CPU/GPU/NPU 推理执行适配 |
| 大模型提供者 | LlmProvider | 有预算、取消、校验和额度状态的总结接口 |

Rust/Python 用 snake_case 函数、PascalCase 类型；C# 按 .NET 惯例；原生 C ABI 函数
前缀 videosift_，C++ 类型 PascalCase、函数 snake_case。JSON/SQL 一律 snake_case。
跨语言类型由 contracts 驱动，公共字段不能自行改名/改义。
UTC 使用带 Z 的 RFC 3339 字符串；媒体时间用非负整数毫秒、半开区间 [start_ms,end_ms)。

## 工具与类型约束

- Rust：rustfmt + Clippy；禁止公共路径 unwrap/expect 处理可预期输入错误；错误带上下文。
  unsafe 集中在明确模块，写 Safety 文档并测试；Phase 0 无 unsafe。
- Python：Ruff、类型注解、Pydantic 用于 Worker 边界；不能在热路径用 Python 重写原生算法。
- C#：Directory.Build.props 及 .editorconfig，nullable 开启、异步 CancellationToken 贯通，
  不使用 async void（UI 事件除外）；ViewModel 放行为，code-behind 仅视图交互。
- C++：RAII（Resource Acquisition Is Initialization）、clang-format，异常不得越过 C ABI。
- 锁定依赖版本和工具链时附兼容性证据；不能用未来版本猜测填配置。

## 错误及日志

分类固定：Validation、Network、Authentication、Source、Media、Inference、Provider、
RateLimit、Storage、Configuration、Cancellation、Internal。wire 使用对应 snake_case。
公共错误含 code、message、task_id（无任务则 null）、recoverable、可选安全 details。
JSON-RPC 外层 code 使用整数，业务 code 在 error.data.code，详见契约。
recoverable 表示可恢复，不表示自动重试；认证、限额和配置错误须由用户处理。

所有外部调用有超时、取消与有限重试策略。错误不能吞掉，也不能把异常当空成功。
UI 显示可操作的中文说明；内部日志结构化保留诊断。
日志字段：timestamp、level、component、event、task_id、duration_ms（适用时）。
等级 TRACE/DEBUG/INFO/WARN/ERROR；Worker stdout 只传 JSONL，日志写 stderr。
默认不记录原始视频正文、提示词、Cookie、Token、完整 URL 查询参数和个人文件路径。
诊断 bundle 须显式导出并脱敏，异常消息不可直接拼接网络响应体。

## 审查清单

行为与文档一致；检查失败不隐藏；契约同步；安全边界明确；不重复计算；没有全局配置改动；
所有新增文件有实际用途；每阶段报告实际执行命令。检查命令集中在 CONTRIBUTING，避免漂移。
