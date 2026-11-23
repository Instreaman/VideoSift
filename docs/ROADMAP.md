# 阶段路线图与验收

每阶段：读取规范 → 检查工作区/远程 → 阶段分支 → 实现 → 实测 → 文档/审查 →
Commit/同步/PR/CI → 等待用户验收。不得未经验收进入下一阶段，不自动合并 main。
Phase 0、Phase 01 及 Phase 02 首项 [Bilibili 离线输入与分 P 校验](BILIBILI_INPUT.md)已验收。
Phase 02 第二项[本地文件来源校验](LOCAL_INPUT.md)和第三项[Bilibili 元数据获取与分 P 校验](BILIBILI_METADATA.md)已验收。
第四项[Bilibili 字幕发现与获取](BILIBILI_SUBTITLES.md)已验收。
第五项[Bilibili 无字幕时的音轨回退](BILIBILI_AUDIO.md)已验收。平台兼容性仍待另行授权后验证。
第六项[来源适配整合与整体验证](SOURCE_INTEGRATION.md)已实现，等待验收。

| Phase | 交付边界 | 进入/验收标准 |
| --- | --- | --- |
| 0 架构与环境 | 环境审计、规范、协议/Schema、校验、README、阶段 PR | 文档/Schema 一致；有效/无效 fixtures 校验；报告限制；用户验收 |
| 1 Rust 核心 | core/cli、有界调度、状态恢复、SQLite 迁移/缓存/FTS5 基线 | Rust/MSVC 可编译；取消/背压/重启/缓存损坏测试；中文短词检索实测；独立 CLI，无 GUI 依赖 |
| 2 Source | local/Bilibili 适配、BV/URL/分 P、字幕优先、合法音轨回退 | 有字幕/无字幕/多 P/超长输入 fixtures；超时、429、认证、结构变化；真实访问显式授权 |
| 3 AI Worker | JSONL Worker、VAD/ASR/OCR、模型复用/释放 | wheels/模型许可核验；崩溃/超时/取消/版本不匹配；CPU ASR/OCR 精度与 RTF/RSS 基线 |
| 4 媒体 | FFmpeg 管道、局部变化关键帧、时间对齐 | 异常媒体、管道背压、子进程树终止；局部代码/公式变化回归；QSV 可用则对照 CPU |
| 5 LLM/知识 | Mock/Codex Provider、上下文预算、结构化摘要/笔记/证据 | 真实调用先过隔离门禁；超时/限额/坏 JSON/Prompt Injection/伪造证据；一次真实调用上限显式启用 |
| 6 Desktop | .NET 10/Avalonia、五页面、MVVM、中文/主题/原生 Markdown | SDK/包兼容；GUI 与 CLI 独立；虚拟化/键盘/屏幕阅读器/对比度/缩放；无 WebView |
| 7 系统集成 | 全模式流水线、契约跨语言往返、导出和恢复 | Windows 全链路合成与授权媒体；所有异常矩阵；Linux/macOS 核心 smoke，失败如实记载 |
| 8 性能优化 | CPU/GPU/NPU 可用性/基准与瓶颈优化 | 基线与优化同条件；RTF/CER/OCR/RSS/吞吐/缓存/token/时间戳/覆盖率；不降质换跑分 |
| 9 发布准备 | 安装/升级/卸载、依赖许可、安全/签名计划 | 干净机器验证、最小权限、配置迁移、离线回退；无授权不建 Release/公开发布 |
| 10 最终审查 | 代码质量、架构一致性、可复现/安全/系统验收 | 全部检查与缺陷关闭或明确接受；真实性能报告，用户决定发布 |

## 横跨阶段的异常矩阵

| 用例 | 首次主要负责阶段 | 后续复验 |
| --- | --- | --- |
| 非法输入、JSON、协议不匹配 | 0 契约；1 运行时 | 3、7 |
| 有字幕/无字幕/多 P/超长视频、网络超时 | 2 | 7 |
| Worker 崩溃、进程退出、任务取消 | 1/3 | 4、7 |
| 缓存失效、数据损坏、磁盘不足、重启恢复 | 1 | 7 |
| LLM 超时/额度/输出错误/注入 | 5 | 7、10 |
| 异常媒体、局部关键帧变化、时间戳漂移 | 4 | 7、8 |
| 凭据和正文泄漏、路径穿越 | 每阶段 | 7、10 |

## 验收报告格式

每阶段报告 Overview、Implementation、Changed Files、Validation、Performance、Security、
GitHub（Branch/Commit Hash/Push Status/PR URL/CI Status）、Known Limitations、Next Phase。
失败和未执行项必须列出，不用“所有测试通过”概括没有执行的项目。性能目标见 BENCHMARK。
