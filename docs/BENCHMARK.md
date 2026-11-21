# 测量计划

**Phase 0 未执行应用性能测试，以下均为待验证目标。** 没有媒体/模型/应用，因此没有 RTF、
RSS、吞吐或 token 成本结果；机器总内存不是应用内存指标。

| 指标 | 定义/方法 | 初步目标或判定 |
| --- | --- | --- |
| Core idle memory | 核心进程稳定 60 秒后 RSS/Windows Working Set；同时记 private bytes | <=50 MB（十进制） |
| GUI + core idle | 两进程相同窗口相加；同时报告 shared pages 可能重复统计 | <=150 MB，Worker 未加载 |
| Idle CPU | 稳定 60 秒，报告均值/p95，单核归一化与整机占比注明 | 接近 0%，不伪造固定阈值 |
| ASR RTF | 推理 wall time / 音频时长；冷启动、VAD、I/O 另列及端到端总值 | <=0.1，且 CER 不明显退化 |
| CER | Character Error Rate，(替换+删除+插入)/参考字符；统一标点/大小写规范 | 先建人工标注基线 |
| OCR accuracy | 字符准确率与关键术语准确率分开；覆盖公式/代码/小字 | 先建标注基线 |
| Peak RSS | 整个进程树按固定采样周期峰值，平台口径分别记 | 按资源预算，不与语言堆大小混淆 |
| Cold start | 创建进程到就绪、模型加载到首结果分别计时 | 报告 p50/p95 |
| Throughput | 同一并发/质量配置下的视频小时/墙钟小时 | 说明 I/O、推理、Provider 各瓶颈 |
| Cache hit rate | 可复用命中/可缓存请求；冷/热/失效场景分别测 | 命中必须验证正确性 |
| Token/video hour | Provider 报告 input/output token / 视频小时 | 缺使用量记 unknown；含重试/分段成本 |
| Timestamp error | 预测边界与人工边界绝对误差 p50/p95；ASR/VAD/OCR 分开 | 先建基线，不假设逐字时间戳准确 |
| Summary coverage | 人工知识点清单召回、证据正确率、幻觉率分开 | 双人或复核标注，记录不确定性 |

## 可复现方法

记录 commit、OS/驱动、电源模式、CPU/GPU/NPU、模型/量化/许可/hash、输入 fingerprint、
采样率、分辨率、时长、语言、线程数、并发、缓存状态、Prompt/Schema 版本。
冷启动至少 3 次；热路径预热后至少 5 次，报告分布和异常值，不只挑最快。
输入为授权可分发样本或合成媒体；真实个人视频不提交。LLM 默认 Mock，真实费用测试显式启用。
对照实验只改变一个因素；INT8 vs 精度、CPU vs 加速、线程数、模型复用均同时检查质量。
输出置于忽略的 .artifacts/benchmarks；审查后的匿名统计再进入 docs。

优化顺序：P0 字幕/按需下载/缓存/INT8/上下文；P1 有界并发/模型复用/去重/线程预算；
P2 QSV/OpenVINO/减少复制；P3 共享内存/FFI/异构调度/原生热点重写。
未达目标时报告实际数据、瓶颈、质量影响和后续建议，不将硬件存在当加速收益。
