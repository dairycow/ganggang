# Introduction

`gg` is a small, sharp agentic harness written in Rust: one agent loop, seven tools, and a terminal UI. It is modelled on [pi](https://github.com/earendil-works/pi) and built to be read.

- **One loop.** Request → tool calls → results → repeat until the model answers. Tools run sequentially, responses stream, `Esc` aborts. There is no turn cap; you are the runaway check.
- **Seven tools.** `read`, `bash`, `edit`, `write`, `grep`, `find`, `ls`. Every truncation is visible in the tool output, and tool failures are values the model reacts to, never crashes. See [Tools](guide/tools.md).
- **Local first.** By default `gg` talks to a local [Strata](https://github.com/Niko1221/Strata) server: no cloud, no accounts, no telemetry. It also speaks OpenAI Chat Completions and Anthropic Messages endpoints, with built-in presets and custom providers. See [Configuration & Providers](guide/configuration.md).
- **Three interfaces on one core.** A fullscreen [TUI](guide/tui.md), a pipeable print mode, and a JSON event stream. See [Output Modes & Export](guide/modes.md).
- **No permission prompts.** `gg` does not ask before running tools. Read the [Security Model](guide/security.md) before pointing it at untrusted work.

## About this guide

This site tracks the `main` branch. Check what you have with `gg --version`. The pages under **Reference** are generated from the code and cannot drift; the rest is checked against the code by the test suite (see [Keeping Docs Up to Date](maintaining-docs.md)). The design rationale lives in [`design.html`](https://github.com/dairycow/ganggang/blob/main/design.html) in the repository.

Start with [Installation](installation.md), then the [Quick Start](quickstart.md).
