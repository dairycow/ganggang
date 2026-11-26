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

### Cloud: glm-coding-plan (default)

With `ZHIPU_API_KEY` set, `gg` starts on the `glm-coding-plan` preset with the `glm-5.3-flash` model. Export the key and go:

```sh
export ZHIPU_API_KEY=...         # Z.ai coding plan
gg --list-models                 # what is available, and which keys are set
```

Other cloud providers: export the key and select the provider.

```sh
export DEEPSEEK_API_KEY=...      # deepseek
export OPENCODE_API_KEY=...      # opencode-go
export OPENROUTER_API_KEY=...    # openrouter

gg -P deepseek
```

`gg --list-models` marks the active model with `*` and shows `ok`, `no key`, or `local` for each row. The full table is in [Providers & Models](reference/providers-and-models.md).

### Local: Strata

```sh
strata launch
curl -s 127.0.0.1:8080/health
gg -P strata                 # only needed while you hold a key for glm-coding-plan
```

`gg` uses Strata whenever you hold no key for `glm-coding-plan` (`ZHIPU_API_KEY`, `GG_API_KEY` or one in your config), and says so on stderr when that passes it over. With a key held, `-P strata` selects Strata.

`gg` expects Strata at `http://127.0.0.1:8080/v1`, auto-adopts whichever model Strata has loaded, and reads `context_window` from `/health` (unless overridden in config or via `-m`/`GG_MODEL`). `gg --list-models` also probes `GET /v1/models` on `strata` and any local OpenAI-compatible provider (such as `llama-server --models-preset`) to list available models and their status, and the TUI's `/model` list refreshes from the same probe in the background. See [Configuration](guide/configuration.md).
