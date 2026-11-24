---
name: style-guide
description: "Write and edit ganggang user guides, README, CLI help text, release notes, and documentation in concise, professional Australian English with no em dashes and no agentic filler. Use whenever creating or revising any user-facing prose in this repository."
---

# ganggang Style Guide

## Scope and voice

Applies to `docs/src/` (guide and introductory pages), `README.md`, user-facing CLI help strings that generate `docs/src/reference/`, release notes, and HTML reports.

- **Voice:** professional, direct, concise. Use active voice, plain verbs, and short sentences.
- **Person:** use second person ("you") for instructions and third person for `gg` behaviour.
- **Precision:** state what `gg` does and its actual limits. Document non-behaviour only when a reader would reasonably expect the opposite (for example, no permission prompts, or `ls` is not recursive).

## Australian English spelling

Follow the Macquarie Dictionary and the Australian Government Style Manual.

| Use (Australian English) | Avoid (US English) | Notes |
|---|---|---|
| `colour`, `colours`, `coloured` | `color`, `colors`, `colored` | |
| `behaviour`, `behaviours` | `behavior`, `behaviors` | |
| `summarise`, `summarised`, `summariser` | `summarize`, `summarized`, `summarizer` | |
| `catalogue` | `catalog` | |
| `materialise`, `materialised` | `materialize`, `materialized` | |
| `synthesised` | `synthesized` | |
| `organise`, `organised` | `organize`, `organized` | |
| `customise`, `customisable` | `customize`, `customizable` | |
| `normalise`, `serialise`, `optimise`, `initialise` | `normalize`, `serialize`, `optimize`, `initialize` | |
| `centre`, `centred` | `center`, `centered` | |
| `cancelled`, `cancelling` | `canceled`, `canceling` | |
| `modelled`, `labelled` | `modeled`, `labeled` | |
| `grey` | `gray` | |
| `focused`, `focusing` | `focussed`, `focussing` | Single `s` |
| `program` | `programme` | Use `program` for software |
| `disk` | `disc` | Use `disk` for computer storage |
| `licence` (noun) / `license` (verb) | `license` (noun) | |
| `practice` (noun) / `practise` (verb) | `practice` (verb) | |

### Exact-match exceptions

Never alter code identifiers, TOML keys, CLI flags, environment variables (`COLORTERM=truecolor`), JSON wire field names, verbatim error messages, or proper nouns (`MIT License`).

## Punctuation, numbers, and typography

- **No em dashes (U+2014):** never use em dashes in prose, headings, tables, or user-facing help strings. Replace them with:
  - a colon (`:`) to introduce an explanation or list;
  - commas or parentheses for brief asides;
  - a semicolon (`;`) to join closely related clauses;
  - a full stop to split a long sentence into two.
- **Hyphens and ranges:** hyphenate compound modifiers before a noun (`read-only`, `append-only`, `one-shot`, `dark-themed`). Use plain words (`1 to 8`, `61% to 85%`) instead of en dashes in prose.
- **Serial (Oxford) comma:** omit by default; include only when needed to prevent ambiguity.
- **Quotes, units, and glyphs:** use straight quotes in Markdown. Attach `%` to numerals (`60%`, `85%`). Put a space before unit symbols (`100 ms`, `30 s`, `50 KiB`, `10 KiB`) and use binary units (`KiB`, `MiB`) to match the implementation. Do not use decorative emoji in technical docs; use Unicode glyphs (`✓`, `✗`, `▸`) only when quoting literal TUI output.

## Eliminating agentic prose (claudespeak)

- **Cut negative parallelism and dramatic contrast:** state the concrete behaviour directly instead of "X, never Y", "not X, Y", or "never a hang, never an unbounded loop".
  - *Avoid:* "Tool failures are values the model reacts to, never crashes."
  - *Use:* "Tool failures are returned to the model as values rather than crashing the run."
- **Cut em-dash parenthetical chains and reveal clauses:** split or punctuate cleanly.
- **Cut personification and whimsical phrasing:** replace "owns the screen", "lends the terminal", "crowds the context window", "rides along", and "a turn that falls short" with plain technical phrasing ("sets the full terminal background", "opens your external editor", "nears the context limit", "truncated responses").
- **Cut defensive design narration:** state the rule once without justifying cleverness (omit phrases like "so a recall never costs a draft" or "it is what you asked for, so nothing asks the model to continue it").
- **Cut rhetorical questions and filler openers:** avoid "Crash mid-run?", "Do this | To", "Simply", "Just", "Easily", "Note that", and "It is worth noting that".
- **Banned hype and filler words:** `delve`, `leverage`, `seamless`, `robust`, `comprehensive`, `streamline`, `empower`, `cutting-edge`, `effortlessly`, `first-class`, `under the hood`, `out of the box`, `at a glance`, `deep dive`, `game-changer`, `tapestry`, `landscape`.
- **Keep internal spec citations out of user docs:** do not include `§09`, `§15`, or `§22` section references from `design.html` in `README.md`, `docs/src/`, or CLI help.

## Documentation structure and contracts

- **`README.md`:** short project overview, installation, quick usage, and a link to the published guide. Do not duplicate the full user guide in `README.md`.
- **`docs/src/guide/`:** topic-oriented manual. Use sentence-case headings (`Interactive TUI`, `Watch mode`, `Truncated responses`) and direct table headers (`Key | Action`, `Command | Description`, `Variable | Effect`).
- **`docs/src/reference/`:** generated from Rust code via `./scripts/sync-docs.sh`. Never edit these files by hand. To fix prose in generated reference pages, edit the source strings (`src/main.rs`, `src/agent/tools/*.rs`, `src/tui/suggest.rs`, `src/doc_sync/generate.rs`) and run `./scripts/sync-docs.sh`.

### Preserve `src/doc_sync/contracts.rs` invariants

When editing documentation, keep these automated checks passing:

- All `GG_*` and provider API key environment variables stay documented in `docs/src/guide/configuration.md`.
- All `HookEvent` variants stay named in `docs/src/guide/hooks.md`.
- All `AgentEvent` JSON `type` strings stay named in `docs/src/guide/modes.md`.
- All `KeyAction` key labels stay named in `docs/src/guide/tui.md`.
- TOML blocks tagged `# ~/.gg/config.toml` or `# ~/.gg/hooks.toml` remain valid TOML accepted by the real loaders.
- Every page in `docs/src/` stays listed in `docs/src/SUMMARY.md` with working relative links.

## Verification workflow

Run these checks after editing documentation:

```sh
grep -rn "—" docs/src/ README.md || true
grep -rniE '\b(color|colors|colored|behavior|behaviors|summarize|summarized|summarizing|summarizer|catalog|materialize|materialized|materializes|synthesized|synthesize|organize|organized|initialize|initialized|customize|customizing|center|centered|canceled|canceling|analyze|analyzed|normalize|normalized|serialize|serialized|optimize|optimized|gray|modeled|modeling|labeled)\b' docs/src/ README.md || true
./scripts/sync-docs.sh
cargo test
```
