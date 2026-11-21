# 任务状态机

Phase 01 的首个子任务提供 `videosift-core::task`。`TaskState` 对应公共 Schema 中的
`ProcessingTask.state`，序列化使用 snake_case。`TaskEvent` 是 Rust 内部事件。
当前只实现状态字段及转换规则，完整 ProcessingTask DTO 和 RPC 接入随调度器实现。

## 转换规则

| 事件 | 起始状态 | 结果 |
| --- | --- | --- |
| Dispatch | queued | running |
| RequestPause | queued | paused |
| RequestPause | running | pause_requested |
| RequestPause | pause_requested、paused | 原状态，重复暂停幂等 |
| PauseAcknowledged | pause_requested | paused |
| Resume | paused、waiting_provider、interrupted | queued |
| Resume | queued、running | 原状态，重复恢复幂等 |
| RequestCancel | running、pause_requested | cancel_requested |
| RequestCancel | queued、paused、waiting_provider、interrupted | cancelled |
| RequestCancel | cancel_requested、cancelled、succeeded、failed | 原状态，重复取消幂等 |
| CancellationAcknowledged | cancel_requested | cancelled |
| Complete | running、pause_requested | succeeded |
| Fail | running、pause_requested | failed |
| ProviderRateLimited | running、pause_requested | waiting_provider |
| RecoverAfterRestart | running、pause_requested、cancel_requested | interrupted |
| RecoverAfterRestart | 其他状态 | 原状态 |

表外组合返回 TransitionError，携带起始状态和内部事件。函数无副作用。
cancelled、succeeded、failed 为终态。interrupted 可由用户恢复。
queued 状态可直接暂停或取消，调用方必须保证 queued 任务尚未拥有运行中的工作。
ProviderRateLimited 在暂停待确认期间发生时进入 waiting_provider，保留限额原因。

## 调用方责任

调度器须串行处理单任务事件，读取当前状态、计算转换并在同一事务中提交。重试恢复后，
调度器还须按 attempt 或内部执行标识拒绝上一轮迟到消息，不能仅按状态判断消息归属。
本模块的纯函数和枚举自身不提供并发锁、数据库事务或进程终止。

处理 `Complete` 前先暂存并校验全部输出。调度器在同一事务中读取当前状态并裁决转换，
接受 `Complete` 时将输出引用、检查点和 `succeeded` 一起提交，事务提交后结果才可见。
若取消已先被接受，拒绝 `Complete`，暂存结果保持未发布。成功事务先提交时，后续取消请求
幂等返回 `succeeded`。暂停在步骤边界确认，任务在确认暂停前完成全部工作时仍可成功结束。

取消先被接受后也会拒绝 `Fail` 和限额消息。调用方继续执行取消清理，完成后发送
`CancellationAcknowledged`。清理失败时保留 `cancel_requested`，由后续进程监管负责
超时终止和报告。重启恢复将此状态转为 `interrupted`，允许用户裁决后续操作。

Fail 和 ProviderRateLimited 的调用方须同时提交符合公共契约的 Error，特别是
waiting_provider 对应 rate_limit，且 error.task_id 与任务一致。状态枚举的反序列化
只检查状态名称，完整 DTO 的校验继续由共享契约约束。

## 本子任务验收

- 10 个状态与 10 个事件的全部 100 个组合符合转换表。
- 暂停确认、重复请求、取消与成功的两种顺序、限额后显式恢复、重启恢复通过测试。
- 终态不会被任何事件改写，非法转换返回明确错误。
- Rust 状态集合与 domain.schema.json 完全一致，复用有效 fixtures 检查状态字段往返。
- 本地 Windows MSVC 构建、fmt、Clippy、测试及 Windows/Ubuntu CI 通过。

后续子任务先完成有界队列与 Mock Worker/Mock Provider 调度，再实现进程监管、SQLite
事务恢复和缓存。每个子任务独立验证，阶段完整验收仍按 ROADMAP 执行。
