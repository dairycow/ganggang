# Introduction

`gg` is a small, sharp agentic harness written in Rust: one agent loop, nine tools, and a terminal UI. It is modelled on [pi](https://github.com/earendil-works/pi).

- **One loop.** Each turn sends a request, executes tool calls, records results, and repeats until the model finishes. Contiguous read-only tool calls run concurrently, responses stream, `Esc` or `SIGTERM` aborts the run, and `--max-turns` caps iterations.
- **Nine tools.** `read`, `bash`, `edit`, `write`, `grep`, `find`, `ls`, `outline`, `task`. Every truncation is visible in the tool output, and tool failures are returned to the model as values rather than crashing the run. See [Tools](guide/tools.md).
- **Local first.** By default `gg` talks to a local [Strata](https://github.com/Niko1221/Strata) server: requiring no cloud service, account, or telemetry. It also speaks OpenAI Chat Completions and Anthropic Messages endpoints, with built-in presets and custom providers. See [Configuration and providers](guide/configuration.md).
- **Three interfaces on one core.** A fullscreen [TUI](guide/tui.md), a pipeable print mode, and a JSON event stream. See [Output modes and export](guide/modes.md).
- **No permission prompts.** `gg` does not ask before running tools. Read the [Security model](guide/security.md) before pointing it at untrusted work.

## About this guide

This site tracks the `main` branch. Check what you have with `gg --version`. The pages under **Reference** are generated from the code and stay in sync with the code; the rest is checked against the code by the test suite (see [Keeping docs up to date](maintaining-docs.md)).

Start with [Installation](installation.md), then the [Quick start](quickstart.md).
