# VideoSift

面向 Windows 原生桌面的视频知识提取引擎，以 Rust 核心、Avalonia 界面和独立 Python 推理进程组成模块化单体。

[English](README.md)

## 当前状态

**Phase 01 SQLite 事务、迁移与重启恢复已验收。** 尚无可运行的应用、视频适配器、AI Worker 或 GUI；没有发布安装包，性能目标尚未测量。

仓库已提供架构边界、安全规范、分阶段验收标准、JSON Schema 草案、JSON-RPC 示例及可执行校验、统一代码与视觉规范，以及不依赖账号凭据的 Phase 0 CI 配置。

计划实现（Planned）：Bilibili 与本地视频、字幕优先与本地语音识别、关键帧与文字识别、带时间戳证据的知识笔记、Fast/Standard/Deep 模式、Codex CLI 总结、SQLite 检索、内容寻址缓存、原生桌面及实测硬件优化。

已提供 Rust core 库的任务状态转换、状态字段序列化、[有界 Mock 调度](docs/SCHEDULER.md)和[进程监管](docs/PROCESS_SUPERVISION.md)。规则和边界见
[任务状态机](docs/TASK_STATE.md)和 [SQLite 任务存储](docs/STORAGE.md)，构建检查见 [开发说明](CONTRIBUTING.md)。

## 验证 Phase 0

使用 Python 3.12+ 和 [requirements-dev.txt](requirements-dev.txt) 中的 jsonschema：

```text
python scripts/validate_repository.py
python -m unittest discover -s tests -v
```

隔离环境配置见 [CONTRIBUTING](CONTRIBUTING.md)。这些命令只验证仓库及契约，不代表应用功能已完成；无需 ChatGPT 登录、付费 API、媒体下载或 GPU。

## 设计约束

Rust 独占任务调度和持久化；C#/.NET 10/Avalonia 负责展示；Python 在受监管进程中推理；媒体处理复用 FFmpeg 和成熟原生库。应用不依赖 WebView、Electron、Node.js 后端、WSL2 或 Docker。

默认 LLM（Large Language Model，大语言模型）提供者计划接入已登录的 Codex CLI；可用性取决于账号及当时额度。开发使用 Mock，真实测试须显式启用并限制调用数。不保证订阅额度、模型可用性或未经测量的性能。

## 文档入口

- [架构](docs/ARCHITECTURE.md)与 [IPC 契约](contracts/README.md)
- [路线图](docs/ROADMAP.md)、[决策](docs/DECISIONS.md)
- [代码风格](docs/STYLE_GUIDE.md)、[视觉规范](docs/DESIGN_SYSTEM.md)
- [安全](docs/SECURITY.md)、[性能测量计划](docs/BENCHMARK.md)
- [开发说明](CONTRIBUTING.md)

阶段 PR 通过后须维护者确认合并及下一阶段。

## 许可证

Copyright 2026 Instreaman. 本项目采用 [Apache License 2.0](LICENSE)。
第三方依赖继续遵循各自许可证。
