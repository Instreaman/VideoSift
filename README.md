# VideoSift

Windows-first native video knowledge extraction, designed around a Rust core,
an Avalonia desktop client and isolated Python inference workers.

[简体中文](README.zh-CN.md)

## Status

**Phase 01 accepted.** The standalone Rust CLI
runs a synthetic Worker/Provider pipeline. Build with `cargo build --locked -p videosift-cli`
and run `videosift --data-dir <existing-private-directory> run`. See the
[command and exit-code reference](docs/CLI.md). The core also supports
[offline Bilibili input parsing and page selection](docs/BILIBILI_INPUT.md) using a caller-supplied catalog.
It can [validate a selected local file](docs/LOCAL_INPUT.md) and identify its contents with BLAKE3.

Present in this repository:

- Architecture, ownership boundaries, security rules and phased acceptance criteria.
- Shared JSON Schema drafts, JSON-RPC examples and executable contract checks.
- Code style, desktop design tokens and a credential-free Phase 0 CI workflow.
- A Rust core library with tested task lifecycle transitions and shared state serialization.
- Verified stage caches and a [reproducible Chinese substring-search comparison](docs/CACHE_SEARCH.md).

See [SQLite task storage](docs/STORAGE.md), [process supervision](docs/PROCESS_SUPERVISION.md), [bounded Mock scheduling](docs/SCHEDULER.md), [task state rules](docs/TASK_STATE.md) and [Rust checks](CONTRIBUTING.md).

Planned: Bilibili retrieval and media inspection; subtitle-first transcription; ASR/OCR;
timestamped evidence; Fast/Standard/Deep processing; Codex CLI summaries; integrated
transcript search; native desktop UI; measured acceleration.

## Validate Phase 0

Use Python 3.12+ with `jsonschema` from [requirements-dev.txt](requirements-dev.txt):

```text
python scripts/validate_repository.py
python -m unittest discover -s tests -v
```

See [CONTRIBUTING](CONTRIBUTING.md) for isolated setup and later language checks.
These commands validate contracts and repository consistency, not application behavior.
No ChatGPT login, paid API, media download or GPU is required.

## Design

Rust is the sole scheduler and persistence owner. C#/.NET 10/Avalonia presents
results; Python performs inference in supervised processes. FFmpeg and mature
native libraries handle media computation. No web frontend or container runtime
is part of the application architecture.

The planned default LLM provider uses an authenticated Codex CLI; access depends
on the user's account and current limits. Development uses mocks. Actual LLM
tests require explicit opt-in and a strict call budget; subscription access and
performance are not guaranteed by this project.

## Project documents

- [Architecture](docs/ARCHITECTURE.md) and [IPC contracts](contracts/README.md)
- [Roadmap](docs/ROADMAP.md), [decisions](docs/DECISIONS.md)
- [Style guide](docs/STYLE_GUIDE.md), [design system](docs/DESIGN_SYSTEM.md)
- [Security](docs/SECURITY.md), [benchmark plan](docs/BENCHMARK.md)
- [Contributing](CONTRIBUTING.md)

Phase branches require maintainer approval before merge.

## License

Copyright 2026 Instreaman. Licensed under the [Apache License 2.0](LICENSE).
Third-party dependencies retain their own licenses.
