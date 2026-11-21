# VideoSift 设计系统

状态：0.1.0 提案。Phase 0 定义资源契约，不创建 GUI 或静态品牌素材。
现代、克制、原生、内容优先、可访问。产品名始终 VideoSift。

## Design Tokens（设计变量）

以下是将来 Avalonia 资源字典的唯一初值来源；共享变量变更须先更新本文件并说明影响范围。
所有页面使用 DynamicResource，不逐页硬编码；图表也用同一状态色。

| Token | Light | Dark |
| --- | --- | --- |
| Color.Background | #F5F6F8 | #202225 |
| Color.Surface | #FFFFFF | #292C30 |
| Color.TextPrimary | #20252B | #F1F3F5 |
| Color.TextSecondary | #4B5563 | #BCC4CE |
| Color.Border | #687380 | #929CA8 |
| Color.Accent | #355F8C | #9BBFE5 |
| Color.OnAccent | #FFFFFF | #202225 |
| Color.Success | #25643D | #8FCDA8 |
| Color.Warning | #785400 | #E3BF67 |
| Color.Error | #A23030 | #EEAAAA |
| Color.Info | #355F8C | #9BBFE5 |
| Color.Focus | #355F8C | #9BBFE5 |

正文与文本背景对比度目标至少 4.5:1，交互边界/焦点至少 3:1。
Phase 6 需验证所有组件实际组合、hover/pressed/disabled、系统高对比度及缩放；
色值表不等于 GUI 可访问性已验收。状态不能只依赖颜色，应附图标与文字。

| 类别 | 统一规则 |
| --- | --- |
| 间距 | Space.4/8/12/16/24/32，值为对应 DIP（Device-Independent Pixel） |
| 圆角 | Radius.Control = 4，Radius.Panel = 4；避免大圆角卡片 |
| 字体 | Windows 系统 UI 字体（中文跟随系统回退），跨平台使用系统字体；代码使用系统等宽字体 |
| 字号 | Text.Caption = 12，Text.Body = 14，Text.Section = 18，Text.Title = 24 DIP |
| 高度 | Control.MinHeight = 32；可访问大尺寸模式 40 |
| 焦点 | Focus.Thickness = 2，键盘焦点始终可见 |
| 动效 | 默认无装饰动画；必要反馈 <=150ms，尊重系统减少动画设置 |
| 图标 | 首次实现时统一选一套单色线形图标并核验许可，禁止混搭 |

## 信息架构

| 页面 | 用户任务 | 主体布局 |
| --- | --- | --- |
| Overview / 概览 | 添加视频、继续最近任务 | 明确入口与最近条目，不堆仪表盘卡片 |
| Tasks / 任务 | 排队、暂停、取消、恢复、查看失败 | 虚拟化列表 + 单项详情，进度含阶段与可用操作 |
| Library / 知识库 | 搜索、阅读、对照时间轴证据、导出 | 查询栏、结果列表、阅读区；证据可定位 |
| Performance / 性能 | 理解资源占用与实测指标 | 预算/读数表格，必要时再引入 ScottPlot |
| Settings / 设置 | 模型、预算、主题、语言、缓存、Provider | 分组表单；敏感内容不回显 |

统一左侧导航与内容区；最小宽度、窄窗口退化由 Phase 6 实测确定。
默认简体中文，字符串从资源读取，保留 en-US；禁止 ViewModel 拼接不可本地化长句。
主题 Light/Dark/System，遵循系统高对比度；偏好由 core 管理。

## 组件与状态

每页面最多一个突出的主按钮，次要动作中性、危险操作显式标识。输入错误靠近字段并保留输入。
表格统一行高、列头、排序和键盘导航；长列表虚拟化。加载只显示真实已知进度，未知阶段用
不定进度，禁止编造百分比。空状态说明原因和下一步；错误状态提供重试条件和诊断编号。
取消不立刻伪装已取消；显示“正在取消”。限额提示保留结果并给手动恢复入口。

时间轴区分字幕/ASR/OCR/视觉证据；时间范围可跳转，低置信度和推断有文字标记。
Markdown 原生渲染，代码可复制，HTML 不执行；外链/配置命令不会自动打开或执行。
公式渲染方案在实际需求时选择安全原生实现；暂不引入浏览器渲染器。
键盘可达、屏幕阅读器名称、焦点顺序、100%/150%/200% 缩放属于 Phase 6 验收。

应用图标、About、CLI 标题、安装包、README 保持名称一致；当前只使用文本标识。
