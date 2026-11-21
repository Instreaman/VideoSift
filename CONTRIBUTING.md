# 开发与 GitHub 工作流

## 阶段门禁

先读 [路线图](docs/ROADMAP.md)、[架构](docs/ARCHITECTURE.md)和本阶段相关规范。只实现获准阶段。
进入新阶段前须获得用户验收和明确授权；不得自动合并、发布或改变仓库可见性。

正常流程（仓库已初始化，不再执行 init）：

```text
git status --short --branch
git remote -v
git branch --show-current
git fetch origin
git rev-list --left-right --count HEAD...origin/main
gh auth status
git switch -c feat/phase-00-architecture origin/main
```

若分支已存在，检查后继续该分支；若工作区不干净，不覆盖、不自动 stash、不 reset。
任何分支名均以实际阶段为准。原子化提交采用 `type(scope): concise English description`。
允许 feat/fix/refactor/perf/test/docs/build/ci/chore/style；禁止模糊的 update、changes。

阶段结束：检查 diff 和敏感数据，更新文档，运行适用检查，提交并 push 阶段分支，
用 `gh pr create` 或已授权 GitHub 连接创建 PR，检查 Actions。PR 使用仓库模板，报告
分支、SHA、push/同步状态、PR URL、CI 结果。禁止 force push、删除已有分支或重写历史。
多行 PR 正文用 `--body-file` 或结构化工具参数。

## Phase 0 校验

使用 Python 3.12+，在项目内隔离安装校验依赖。

```text
uv venv .venv --python 3.12
uv pip install --python .venv/Scripts/python.exe -r requirements-dev.txt
.venv/Scripts/python.exe scripts/validate_repository.py
.venv/Scripts/python.exe -m unittest discover -s tests -v
```

Unix 使用 `.venv/bin/python`。Phase 0 测试用标准库 unittest，AI Worker 建立时引入 pytest。
已安装 Ruff 时执行 `ruff check .`、`ruff format --check .`；未安装须报告跳过，不能宣称通过。
CI 用 Python 3.12 验证最低开发版本，不依赖当前宿主 Python 版本。

## 后续检查矩阵

| 模块出现时 | 必须执行 |
| --- | --- |
| Rust workspace | cargo fmt --check；cargo clippy --workspace --all-targets -- -D warnings；cargo test --workspace |
| Python Worker | uv 环境内 ruff check；ruff format --check；pytest |
| .NET solution | dotnet format --verify-no-changes；dotnet build；dotnet test |
| 自定义原生模块 | clang-format 检查；CMake build；CTest；适用的 clang-tidy |
| 任意跨语言 DTO | 同一 fixtures 的序列化往返、Schema 和语义校验 |

依赖在首次实际使用的阶段锁定版本与锁文件；不为未来模块提前安装依赖或生成空目录。
CI 最小权限 `contents: read`；禁用凭据、真实视频和真实 LLM 测试。硬件及耗时测试手动触发。
README 只列实有功能，未实现能力标记 Planned。仓库描述建议：
`VideoSift: Windows-first native video knowledge extraction engine (architecture phase).`
尚未修改仓库描述时不得称已同步。

## Rust 核心检查

使用 rust-toolchain.toml 固定的工具链。Windows 需要 MSVC C++ 构建工具与 Windows SDK。
workspace 包含 videosift-core 和 videosift-cli。前者提供核心库和合成子进程测试工具，后者生成独立的 videosift 可执行文件。
CLI 的跨进程验收用例随 workspace 测试一起执行，见 [CLI 使用说明](docs/CLI.md)。
状态机见 [TASK_STATE](docs/TASK_STATE.md)，进程验证见 [PROCESS_SUPERVISION](docs/PROCESS_SUPERVISION.md)。

```text
cargo build --workspace --locked
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Cargo.lock 随应用 workspace 提交。Serde 负责状态值序列化，serde_json 也用于缓存键的规范化。
共享契约仍以 contracts 为准，现阶段测试只声明状态字段往返覆盖。

## 本地开发资料

Agent 指令、对话记录、交接进度和机器环境记录留在本地，遵循 .gitignore。
提交内容包括项目源码、测试、协议、构建和 CI 配置以及必要的项目文档。
凭据、个人媒体、运行数据库和模型文件使用本地私有目录保存。
开源贡献遵循仓库 [Apache-2.0 许可证](LICENSE)，第三方依赖继续遵循各自许可。
