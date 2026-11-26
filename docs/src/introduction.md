# Introduction

`gg` is a small, sharp agentic harness written in Rust: one agent loop, eight tools, and a terminal UI. It is modelled on [pi](https://github.com/earendil-works/pi).

- **One loop.** Each turn sends a request, executes tool calls, records results, and repeats until the model finishes. Contiguous read-only tool calls run concurrently, responses stream, `Esc` or `SIGTERM` aborts the run, and `--max-turns` caps iterations.
- **Eight tools.** `read`, `bash`, `edit`, `write`, `grep`, `find`, `ls`, `outline`. Every truncation is visible in the tool output, and tool failures are returned to the model as values rather than crashing the run. See [Tools](guide/tools.md).
- **Bring your own backend.** While you hold a key for it, `gg` talks to the `glm-coding-plan` preset (`glm-5.3-flash`), and to a local [Strata](https://github.com/Niko1221/Strata) server otherwise. OpenAI Chat Completions and Anthropic Messages endpoints, built-in presets and custom providers are all supported. See [Configuration and providers](guide/configuration.md).
- **Three interfaces on one core.** A fullscreen [TUI](guide/tui.md), a pipeable print mode, and a JSON event stream. See [Output modes and export](guide/modes.md).
- **No permission prompts.** `gg` does not ask before running tools. Read the [Security model](guide/security.md) before pointing it at untrusted work.

## About this guide

This site tracks the `main` branch. Check what you have with `gg --version`. The pages under **Reference** are generated from the code and stay in sync with the code; the rest is checked against the code by the test suite (see [Keeping docs up to date](maintaining-docs.md)).

Start with [Installation](installation.md), then the [Quick start](quickstart.md).
