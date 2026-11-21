# 进程管理与取消超时清理

本子任务在 core 内实现 `process` 模块，复用已验收的 Scheduler 和 TaskState。
ProcessSupervisor 是状态和进程回收的单一所有者；Tokio 负责异步管道、计时与退出等待。
当前提供字节传输和生命周期，不实现 Worker JSON-RPC、真实推理或应用 CLI。

## 所有权与完成顺序

1. submit 接纳任务；dispatch 按现有队列派发 Worker/Provider 步骤并启动可信程序。
2. 程序路径必须为绝对路径，参数使用数组，不经 shell 拼接。调用方负责程序授权和参数校验。
3. 输入、输出和 settle（完成回收）均绑定 Execution；旧步骤票据不能影响新步骤。
4. 进程退出、管道结束和进程树清理完成后，settle 才向 Scheduler 提交结果并释放执行槽位。
   当前以退出成功映射 Mock 步骤成功；真实输出仍须经后续协议和语义校验后才能发布。
5. 取消先被 Scheduler 接受时，即使进程成功结果已到达，也只确认取消，不发布成功标记。
   成功已由 settle 接受时，后续取消保持成功终态。取消 settle 的等待不会取消后台清理。

启动失败将对应步骤标为 failed 并回收槽位，错误保留执行票据和 OS 错误类别，不包含路径正文。
清理未确认或后台任务异常时返回明确错误，保留占用槽位，不虚报 cancelled。
管道任务返回错误或异常退出时，ProcessReport 用 pipe_failed 记录。若进程树已退出且所有管道任务
均已结束，仍可完成回收。管道任务超过清理期限时保留槽位。
清理超时的槽位当前需要隔离处理；不提供未经确认的强制释放或自动重试。

## 取消、期限与进程树

- 调用 cancel 先串行更新状态，再通知进程层。合作取消通过关闭 stdin 发出 EOF，丢弃尚未写完的输入。
  这是当前短生命周期测试进程的合作机制，不新增公共 RPC 方法，也不把 task.cancel 转发给 Worker。
- cancel_grace 到期仍未退出时，强制终止进程树。Cancelled.grace_expired 表示根进程是否超过宽限期。
- 根进程正常退出或崩溃后也清理遗留子孙；取消期间输出转为读取后丢弃，避免管道阻塞妨碍合作退出。
- Windows 复用 Job Object：挂起创建、完成归属后恢复，启用 kill-on-close，隐藏控制台窗口。
  增加可查询的外层 Job，在恢复前归属，包含同一进程树；确认 ActiveProcesses 为零才释放槽位。
- Unix 使用独立进程组。`waitid(WNOWAIT)` 观察根进程退出并保留其 PID，监管器独占回收权。
  向该组发送终止信号后，先禁用后续终止操作，再回收根进程。之后只探测组是否存在。
  非直接子进程的 zombie 由收养父进程回收。若未及时回收，或组编号复用使存在性探测无法确认，
  槽位保持隔离。宿主不可并行回收监管器持有的子进程。
- run_timeout 覆盖运行和正常输出排空；cleanup_timeout 限制清理确认等待。
  三种期限均须显式配置，范围为大于零且不超过 24 小时。候选 5 秒取消宽限期仍不是性能承诺。
- 丢弃 supervisor 会请求清理；正常关闭应逐项 cancel/settle 并保持 Tokio 运行时存活。
  运行时突然关闭只能尽力终止。Unix 宿主被 SIGKILL 后的子孙清理、主动逃离进程组的程序尚无保证；
  不能将本模块当作不可信程序的安全沙箱。Windows 另有宿主直接退出、不运行析构函数的 kill-on-close 测试。

## 管道背压与内存边界

stdin 使用有界队列，stdout/stderr 独立读取并共享有界输出队列。每块最多 4096 字节，
pipe_chunks 范围为 1..=1024。try_send_input 满时返回 InputFull，调用方稍后重试。
两条输出管道满时等待消费者，压力传回 OS 管道；不会 read_to_end 收集无限正文。
每个步骤的应用缓冲为两个队列及固定数量的在途块，另有 OS 管道和依赖内部缓冲。

调用方应持续 recv_output，读到 EOF 后 settle；仅等待 settle 而不消费大量输出会触发运行期限。
接受取消时同步丢弃已入队未消费的输出，后续 recv_output 返回 EOF。后台继续排空 OS 管道，
避免子进程在写输出时阻塞。运行超时会停止向队列投递，已入队数据随 settle 回收或由调用方先行读取。
调用方已取得的字节仍是不可信暂存数据，不代表成功发布。
不记录管道正文、命令参数或个人路径。容量与并发仍受 Scheduler 限制，终态记录仍需 take_finished 回收。

## 开源复用与验证

- Tokio 1.54.1：异步进程、I/O、同步和时间；仅开启实际使用的功能。
- process-wrap 10.0.1：进程组、Job Object 和 kill-on-drop；关闭默认特性，仅启用所需封装。
- Windows 0.62.2 / nix 0.31.4：复用同一依赖树中的平台类型和进程组存活检查。
- 版本来自本次实际获取和 Cargo.lock；Tokio/process-wrap 的最低 Rust 版本分别为 1.85/1.87，
  本地固定 Rust 1.100.0 编译通过。没有添加 C/C++ 或手写进程创建实现。
- 源码核查发现 process-wrap 的 Windows 树等待不能单凭完成端口消息证明树已空，因此不调用该等待接口。
  复用其挂起创建/恢复/终止能力，增加一个外层 Job 供活动计数查询，代价为每次执行多一个 Job 句柄。
  使用 Tokio 根进程回收及平台存活查询。Unix 采用间隔 10 毫秒的不回收式退出探测，保留发信号所需的
  进程组身份。平台 unsafe 集中于 tree/job/unix 模块，并记录句柄、结构体缓冲和调用顺序约束。

参考：[Tokio 进程 API](https://docs.rs/tokio/1.54.1/tokio/process/)、
[process-wrap](https://docs.rs/process-wrap/10.0.1/process_wrap/)、
[Unix waitid](https://man7.org/linux/man-pages/man2/waitpid.2.html)、
[Windows Job 活动计数](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_accounting_information)。依赖首次引入时检查来源、许可和对应源码。

验收测试使用 process-test-helper 合成子进程，它只是测试工具，不是应用 CLI：

- 合作取消、宽限期后终止父子进程、根退出后清理子孙、丢弃监管器触发清理。
- stdout/stderr 各 4 MiB 有界传输，无消费者和 stdin 阻塞时仍能取消。
- 运行超时、启动失败、非零退出、非法容量、槽位回收及清理失败保留槽位。
- 多进程取消互不干扰、完成回收前取消、跨步骤旧票据拒绝、完成后的取消幂等。
- 回归测试覆盖已报错与仍未结束的管道任务、取消后丢弃队列中的输出，以及 Unix 回收前后对进程组的终止权限。

本地 Windows 通过 12 个进程集成测试、4 个进程单元测试及原有 16 个测试。
Ubuntu 结果以本子任务 PR CI 为准。没有性能基准、真实 AI 调用、完整 IPC、安全隔离或持久化结论。
