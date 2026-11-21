# 有界队列与 Mock 调度

当前 core 提供同步内存调度器，调用方通过 submit、dispatch、MockWorker/MockProvider.finish
和取消确认驱动显式事件循环。所有变更需要独占 &mut Scheduler，单任务事件按调用顺序裁决。
Tokio I/O 运行时在接入异步工作或进程时引入，本子任务复用标准库集合和已验收状态机。

## 容量与流程

构造参数 capacity 限制全部保留任务数，涵盖就绪、执行、限额等待和终态记录。
concurrency 限制同时派发的步骤数，范围为 1..=capacity。submit 容量不足立即返回 Full，
调用方可在取走终态后重试。take_finished 只取走终态并释放容量。就绪队列长度始终不超过
capacity，每个任务最多保留一个就绪项或一个执行票据。Mock 输出使用固定大小的完成标记。

任务先执行 Worker，再执行 Provider。Worker 成功保留 worker_completed，任务继续为 running，
Provider 步骤加入队尾。派发按 FIFO 选择可运行步骤，Provider 暂停时允许跳过它继续执行 Worker。
步骤结束释放并发槽位，终态记录取走后释放任务容量。concurrency 表示执行步骤数。

TaskHandle 和 Execution 为调度器内部标识。句柄绑定当前调度器实例，执行标识在每次派发时
递增，溢出明确返回错误。重复结果、前一步结果、上一轮限额重试结果，以及其他调度器结果
在任何状态或限额标志变更前被拒绝。公共协议及 ProcessingTask DTO 的接入仍待后续完成。

## 取消与限额

排队、步骤之间或 waiting_provider 的取消可直接完成。正在执行的任务进入 cancel_requested，
直到匹配票据的 acknowledge_cancel 才释放槽位。迟到的成功、失败或限额结果保持被拒绝。
成功先到时，在一次独占操作内更新状态和 result_published，之后取消幂等返回 succeeded。
这套内存发布语义将作为后续 SQLite 原子事务的行为基线。

Provider 返回 RateLimited 后任务进入 waiting_provider，保留 Worker 完成标记，同时关闭
该调度器的 Provider 派发。已在途的 Provider 允许返回结果，Worker 可继续执行。
只有 resume_provider 显式开启派发，并将所有 waiting_provider 任务经状态机恢复为 queued。
重试从 Provider 步骤开始，每次获得新票据。重复恢复不会重复入队。取消等待任务不会自动
开启 Provider。当前模型是一台调度器对应一个 Provider 配额范围。

## 验证与边界

测试覆盖容量和 FIFO、并发上限、终态回收、取消与成功两种顺序、取消确认槽位回收、
Worker 取消、重复和旧轮次结果、跨调度器消息、限额后的 Worker 派发、显式恢复与在途返回。
MockWorker 与 MockProvider 接收可控成功/失败/限额结果，因此测试通过事件顺序直接复现竞争。

Scheduler 自身不执行后台 I/O、媒体处理或持久化。调用方负责驱动事件循环和取消确认。
同一调度器的所有数据由单一所有者访问；纯 Scheduler 测试验证串行裁决，进程集成另行验证。

进程层已在本次子任务接入，具体实现、测试和剩余边界见 [PROCESS_SUPERVISION](PROCESS_SUPERVISION.md)。
上述同步 Scheduler 接口继续作为串行裁决核心，不直接承担 I/O。
