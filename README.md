# VideoSift

Windows-first native video knowledge extraction, designed around a Rust core,
an Avalonia desktop client and isolated Python inference workers.

[简体中文](README.zh-CN.md)

## Status

**Phase 01 is complete. All six Phase 02 source features have been accepted and merged into `main`.**
These features are available through the Rust core library.

- **Bilibili input.** Parse a BV identifier or a standard playback URL offline. Page validation requires a
  caller-supplied catalog. For a multi-page video, the caller must choose a page explicitly. [Input and page selection](docs/BILIBILI_INPUT.md)
- **Local file input.** Validate a selected, nonempty regular file under a caller-authorized directory.
  Its BLAKE3 content digest identifies the source across renames and copies. Media format and playability checks are planned. [File validation and platform limits](docs/LOCAL_INPUT.md)
- **Bilibili metadata.** Fetch metadata over restricted HTTPS and validate the page selection against the
  returned catalog. A multi-page video still requires an explicit choice. [Metadata behavior and limits](docs/BILIBILI_METADATA.md)
- **Bilibili subtitles.** Discover subtitle tracks for the selected page, fetch the caller-selected track and
  validate its timestamps against the page duration. [Subtitle discovery and retrieval](docs/BILIBILI_SUBTITLES.md)
- **Bilibili audio fallback.** Only a successful subtitle discovery response with an explicit empty list
  enables fallback for the same page. The core can fetch one AAC-LC DASH audio track over restricted HTTPS,
  checking that every top-level MP4 box is complete and that at least one media data box (`mdat`) is nonempty.
  [Audio fallback and limits](docs/BILIBILI_AUDIO.md)
- **Source adapter integration.** Local files and Bilibili use the shared `VideoSource` contract. The Bilibili
  coordinator uses the validated catalog and selected page to return subtitle tracks for caller selection
  or to resolve an audio track after confirmed subtitle absence. The caller retrieves the chosen track
  through the existing APIs. [Source adapter workflow](docs/SOURCE_INTEGRATION.md)

Integration tests on Windows and Ubuntu use synthetic files and mock HTTP responses to check path
authorization, source serialization, page selection, request order, and error handling for malformed
responses, access denial, rate limits and timeouts. Live Bilibili access requires separate authorization.
Live platform compatibility, media playability, audio decodability and performance remain unverified.

The standalone Rust CLI runs a synthetic task with a Mock Worker and Mock Provider. The first-run example
shows how to run it twice in the same private data directory and check for cache hits on the second run. [CLI quick start](docs/CLI.md)

Present in this repository:

- Architecture, ownership boundaries, security rules and phased acceptance criteria.
- Shared JSON Schema drafts, JSON-RPC examples and executable contract checks.
- Code style, desktop design tokens and a credential-free Phase 0 CI workflow.
- A Rust core library with tested task lifecycle transitions and shared state serialization.
- Verified stage caches and a [reproducible Chinese substring-search comparison](docs/CACHE_SEARCH.md).

See [SQLite task storage](docs/STORAGE.md), [process supervision](docs/PROCESS_SUPERVISION.md), [bounded Mock scheduling](docs/SCHEDULER.md), [task state rules](docs/TASK_STATE.md) and [Rust checks](CONTRIBUTING.md).

Planned work includes media probing and decoding, a subtitle-first transcript pipeline with local speech
recognition, keyframe extraction and OCR, knowledge notes with timestamped evidence, Fast/Standard/Deep processing, Codex CLI
summaries, integrated transcript search, a native desktop UI and measured acceleration.

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
