# 独立 Rust CLI

`videosift` 使用内置中文合成字幕、Mock Worker 和 Mock Provider 演练任务全流程。
运行需要一个用户私有、已存在、供本 CLI 专用的数据目录。每次调用只打开一个任务库。
构建使用仓库固定的 Rust 工具链；程序直接在本机运行，无需 GUI、模型或网络服务。

## 首次运行

在 Windows PowerShell 中，从仓库根目录执行：

```powershell
cargo build --locked -p videosift-cli
$store = Join-Path ([IO.Path]::GetTempPath()) ("videosift-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $store | Out-Null
./target/debug/videosift.exe --data-dir $store run
./target/debug/videosift.exe --data-dir $store run
```

每次 `run` 创建新任务，返回的 `task_id` 是仅在该目录内有效的十进制字符串。
第一次运行在冷缓存中执行两步 Mock；第二次运行的两个阶段均应报告 `cache_hit: true`，
并发布相同的 `output_ref`。CLI 校验缓存内容，损坏的合成产物须重新生成。
Provider 缓存键绑定 Worker 的产物身份和内容摘要。

| 命令（前置参数为 `--data-dir <目录>`） | 行为 |
| --- | --- |
| `run [--interactive]` | 执行新的合成任务。交互模式提交 Worker 检查点后等待一行输入。 |
| `status <task_id>` | 打开任务库并读出状态、检查点及结果引用。打开时执行正常启动恢复。 |
| `cancel <task_id>` | 在任务库空闲时取消排队或中断的任务，终态保持稳定。活跃的交互运行在原终端输入 `cancel`。 |
| `resume <task_id> [--interactive]` | 使用同一任务编号恢复到队列并执行；已入队的任务也可派发。终态拒绝恢复。 |

## 中断和取消

- 在 `run --interactive` 的 `worker_checkpoint` 事件后输入 `continue` 运行 Provider，
  或输入 `cancel` 并回车确认取消。EOF 或空行也会取消。其他输入取消当前任务并返回输入错误。
- 要验证崩溃恢复，可在该事件后直接终止前台进程。下一次 `status <task_id>` 应返回
  `interrupted`、已提交的检查点和空的 `output_ref`。重复查询保持中断。
  `resume <task_id>` 重新检查缓存，复用已核验的 Worker 输出并继续 Provider。
- 一个前台进程独占数据目录。另一个命令访问相同目录时返回占用错误，不能夺取活跃任务。
  普通 `run` 连续执行两个短 Mock 步骤。交互模式的步骤边界提供确定的合作取消点。

## 输出与退出码

正常输出采用 UTF-8 JSONL，每行一个事件，写完立即刷新。任务快照 `task` 包含
`task_id`、`state`、`checkpoint` 和 `output_ref`；引用为阶段缓存键或 null。
`worker_checkpoint` 表示已持久化 Worker 检查点；`provider_completed` 表示结果引用
已经提交。这两个事件的 `cache_hit` 表示该阶段复用了已核验的缓存。
成功终态、检查点与结果引用由 core 在同一 SQLite 事务中发布。

| 退出码 | 含义 |
| --- | --- |
| 0 | 命令成功，包括查询、显式 cancel 命令及帮助。任务状态以输出为准。 |
| 1 | 存储、独占锁、调度或输入输出错误。 |
| 2 | 命令行参数、交互输入、任务编号或当前状态无效。 |
| 3 | 前台运行在 Worker 检查点接受取消并已确认。 |

帮助、版本及 Clap 参数诊断使用文本。运行期错误在 stderr 输出与公共 Error 字段一致的 JSON；
启动缓存清理待重试时另有 warning 事件。运行事件属于本地 CLI 接口，公共 JSON-RPC 契约沿用现有版本。

## 整体验证

`cargo test --workspace --locked` 启动真实 CLI 子进程断言冷／热缓存、损坏重建、合作取消、
占用拒绝和强制退出后的显式恢复；同时复验 core 的状态、背压、进程树清理、事务、旧票据和缓存测试。
中文检索实测方法和合成语料规模见[检索基线](CACHE_SEARCH.md)。这些检查只使用合成数据。
