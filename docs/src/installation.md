# Installation

## From crates.io

```sh
cargo install ganggang    # installs the `gg` binary
```

## From the latest `main`

The published crate can lag behind `main`. For the newest features:

```sh
cargo install --git https://github.com/dairycow/ganggang
```

## From source

```sh
git clone https://github.com/dairycow/ganggang
cd ganggang
cargo build --release
./target/release/gg
```

`gg` targets Unix-like systems: it uses `bash`, process groups, and `kill`.

## Pick a backend

### Local: Strata (default)

```sh
strata launch
curl -s 127.0.0.1:8080/health
```

`gg` expects Strata at `http://127.0.0.1:8080/v1`, auto-adopts whichever model Strata has loaded, and reads `context_window` from `/health` (unless overridden in config or via `-m`/`GG_MODEL`). `gg --list-models` and `/model` also probe `GET /v1/models` on `strata` and any local OpenAI-compatible provider (such as `llama-server --models-preset`) to list available models and their status. See [Configuration](guide/configuration.md).

### Cloud providers

Export the provider's key and select it:

```sh
export DEEPSEEK_API_KEY=...      # deepseek
export ZHIPU_API_KEY=...         # glm-coding-plan
export OPENCODE_API_KEY=...      # opencode-go

gg --list-models                 # what is available, and which keys are set
gg -P deepseek
```

`gg --list-models` marks the active model with `*` and shows `ok`, `no key`, or `local` for each row. The full table is in [Providers & Models](reference/providers-and-models.md).
