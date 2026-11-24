# Python Worker 握手

连接方使用锁定环境的 Python 执行 `python -I worker/main.py DEADLINE_NS`。
启动前取同机 Python `time.monotonic_ns() + 5_000_000_000` 作为唯一绝对截止时间，
以十进制纳秒传参。连接方须独立守护此期限，收到有效握手响应才解除；
子进程未启动完时由连接方终止。Worker 启动后按剩余预算计时，再加载协议依赖。
Worker 通过 stdin 和 stdout 交换 UTF-8 JSONL；字段与错误结构遵循[共享契约](../contracts/README.md)。
Mock core 可以发送共享样例中的握手请求。成功响应回显请求 id，服务名为 worker，
能力列表为空。本项仅交付 Worker 端点，Rust 应用接入和模型推理尚未验证。

从建立管道连接起 5 秒内须完成握手，部分行同样受限。版本不匹配返回 1009 后关闭。
每帧含 LF 最多 1 MiB，每条连接最多记录 4096 个不同请求 id，到达限额后下一条新请求被拒绝并关闭。
握手成功后正常 EOF 退出码为 0；握手前 EOF、未终止帧、超限、超时或版本不匹配退出码为 2。
进程被外部终止时，监管方应检查 OS 退出状态，将尚未收到响应的请求判为失败。
测试使用独立 Python 子进程、合成输入与 Mock core。执行
`uv run --project worker --locked python -m pytest -q worker/tests`。

## Rust 监管握手

core 的 `worker::probe_worker` 接收可信 Python 解释器和入口的绝对路径，使用
ProcessSupervisor 发起一次握手探测。Rust 从派发前计时，五秒内须收到匹配请求的完整响应。
`--supervised` 表示由 Rust 持有连接期限，Python 自身也有启动后的保护计时。
成功后关闭 stdin，等待正常退出；失败则请求清理进程树，确认回收后才返回协议错误。
本项提供短生命周期探测，能力列表为空。集成测试使用锁定的 Worker 环境，先执行
`uv sync --project worker --locked`，再执行 `cargo test -p videosift-core --test worker`。
