# Python Worker 握手

在仓库根目录执行 `uv run --project worker --locked python -I worker/main.py`。
Worker 通过 stdin 和 stdout 交换 UTF-8 JSONL；字段与错误结构遵循[共享契约](../contracts/README.md)。
Mock core 可以发送共享样例中的握手请求。成功响应回显请求 id，服务名为 worker，
能力列表为空。本项仅交付 Worker 端点，Rust 应用接入和模型推理尚未验证。

协议循环启动后 5 秒内须完成握手，部分行同样受限。版本不匹配返回 1009 后关闭。
每帧含 LF 最多 1 MiB，每条连接最多记录 4096 个不同请求 id，到达限额后下一条新请求被拒绝并关闭。
握手成功后正常 EOF 退出码为 0；握手前 EOF、未终止帧、超限、超时或版本不匹配退出码为 2。
进程被外部终止时，监管方应检查 OS 退出状态，将尚未收到响应的请求判为失败。
测试使用独立 Python 子进程、合成输入与 Mock core。执行
`uv run --project worker --locked python -m pytest -q worker/tests`。
