# IPC 契约草案 0.1.0

IPC = Inter-Process Communication（进程间通信）。这是 JSON-RPC 2.0 的受限 profile，
其中 Python Worker 已实现 stdio 握手子集，见 [Worker 说明](../worker/README.md)。标准依据：[JSON-RPC 2.0](https://www.jsonrpc.org/specification)。
Schema 为 JSON Schema Draft 2020-12；contracts 是跨语言 DTO 的唯一真源。

## 信封与传输

- 标准信封只含 jsonrpc、id、method、params 或 result/error。`id` 即 request_id 的
  语义，不再发 request_id 字段。请求 id 长度为 1–128 个字符，仅允许 ASCII 字母、数字、下划线和连字符。
  响应 id 使用相同限制（标准错误允许 null）；无 id 的通知不得回复。
- protocol_version 在 params/result/error.data 中；task_id 同样放 payload，握手/进程级
  无任务则 null。成功 result 与失败 error 必须互斥，响应回显请求 id。
- 解析失败或无法识别 id 的无效请求，返回 id:null 的标准错误；无 id 的合法通知不返回错误。
- profile 不支持 batch 或位置参数；接收 batch 返回单个 -32600/id:null，不做部分执行。
- Rust↔Python：stdio，UTF-8 JSONL（JSON Lines），每行一个对象，LF 结束，无 BOM。
  字符串里的换行必须转义；stdout 禁日志，stderr 结构化日志有界读取。
- C#↔Rust：Windows Named Pipe byte mode，使用同一 JSONL 分帧。Unix socket 后端保持同一语义。
- 每帧最多 1,048,576 字节（含 LF），拒绝 UTF-8 错误、空行、超限帧与非有限数字。
  超限关闭连接，不分配无限缓冲；媒体禁止 Base64 JSON，使用受控 artifact 引用。
- 当前用户 ACL、远程客户端拒绝、私有 socket 目录要求见 SECURITY。接口名称不是访问控制。

## 握手、版本与请求生命周期

连接后 5 秒内完成 system.handshake，client_name 为 desktop/cli/core/worker，双方交换
protocol_version 与 capabilities。0.1.0 草案要求精确版本匹配，不自动忽略未知字段。
不匹配返回 Configuration 业务错误并关闭连接；其他业务请求在握手前拒绝。
升级先更新 Schema、fixtures、所有消费者，再在 DECISIONS 说明兼容性；未来才引入版本范围协商。

请求 id 在连接生命周期内唯一；重复 id 返回 -32600 且不重执行。连接关闭时所有 pending 请求
明确失败。事件通知可能丢失；重连后 task.get 读取持久状态，不以通知作为事务完成的唯一依据。
Phase 1 的运行时负责请求/响应关联、顺序、权限与 deadline；Schema 不能证明这些性质。

| 方法 | 方向/形式 | 输入/输出 |
| --- | --- | --- |
| system.handshake | 任意客户端→服务端，请求 | 版本/client_name/capabilities；返回版本/server_name/capabilities |
| task.get | GUI/CLI→core，请求 | task_id；返回 ProcessingTask |
| task.cancel | GUI/CLI→core，请求 | task_id；返回最新 ProcessingTask，接受不代表已经取消 |
| task.pause | GUI/CLI→core，请求 | task_id；返回最新状态，步骤边界暂停 |
| task.resume | GUI/CLI→core，请求 | task_id；返回 queued 或当前幂等状态 |
| inference.transcribe | core→worker，请求 | task_id、授权音频 FileReference、language；返回 Transcript |
| task.progress | core/worker→监管方，通知 | task_id、stage、completed、total（未知为 null）；completed<=total |
| process.exiting | 子进程→监管方，通知 | task_id 可 null、exit_code；通知不替代 OS 退出事件 |

提交/查询列表、OCR、关键帧、设置、检索和完整 Summary 方法在对应阶段扩展，不虚构完成的 API。
通用 task.* 仅 core 可处理；Worker 不接受任务调度请求。能力必须与实际实现一致。

控制请求默认 deadline 10 秒；握手 5 秒；transcribe deadline 随媒体时长与预算配置，
不得无限等候。取消合作宽限期候选 5 秒，随后终止进程树。以上是待测默认，不是 SLA。
超时关闭/取消相关工作，迟到结果不能改变终态；部分结果通过 checkpoint 重用。
Schema 验证之后还需状态机、证据引用、媒体时长、授权路径和资源预算校验。

## 错误

标准错误保留 -32700/-32600/-32601/-32602/-32603（parse/invalid request/method/params/internal）。
标准错误可无 data；业务错误使用整数 1000..1011，与 Error.code 一一对应：

| code（整数） | error.data.code |
| --- | --- |
| 1000 | validation |
| 1001 | network |
| 1002 | authentication |
| 1003 | source |
| 1004 | media |
| 1005 | inference |
| 1006 | provider |
| 1007 | rate_limit |
| 1008 | storage |
| 1009 | configuration |
| 1010 | cancellation |
| 1011 | internal |

error.message 为简短安全说明。error.data 是 Error，包含 protocol_version、code、message、
task_id、recoverable、可选 details。外层/内层 message 必须一致；details 仅允许 reason、
retry_after_ms，不携带原始正文/凭据/路径。超时归属正在执行的组件类别，reason=timeout；
版本不匹配用 configuration/reason=protocol_version_mismatch。recoverable 不表示自动重试。

## 公共数据约束

见 [domain.schema.json](domain.schema.json) 的 $defs：VideoSource、Video、FileReference、
ProcessingTask、TranscriptSegment、Transcript、Keyframe、KnowledgeItem、Summary、Error。
wire 字段 snake_case；空值与缺字段不同。媒体时间为整数毫秒半开区间，不依赖墙钟时间。
confidence 是 [0,1] 或 null（未知）；不可捏造置信度。路径引用必须是 artifact 根下的 POSIX
相对路径，实际绝对路径由 core 映射，不可使用 ../、盘符、UNC 或不可信远程 URI。

TranscriptSegment 的 evidence_id 在 Transcript 内唯一。Summary 包含可引用 evidence_ids，
KnowledgeItem 的引用必须属于该集合，video 类必须非空，model_inference/external_knowledge
允许无引用但必须有 uncertainty。运行时还要确认 evidence_ids 确实存在于当前任务的证据仓库，
JSON 自声明列表不能作为真实性证明。未知 duration_ms 用 null；终止时间不能超出已知媒体时长。

FileReference.content_hash 是 64 个小写十六进制字符的 BLAKE3 哈希，文件读入后验证，不相信声明值。
examples 全为人工合成；全零哈希仅测试形状，不代表真实文件存在。
JSON Schema 无法直接比较 start_ms/end_ms 等字段，scripts/validate_repository.py 进行语义校验。
Phase 1/3/6 用同一 fixtures 分别检查 Rust/Pydantic/C# 往返，当前不声称跨语言运行时已兼容。

所有限定字符集的 pattern 都要求整串匹配，包括拒绝末尾换行和 Unicode 行分隔符。
