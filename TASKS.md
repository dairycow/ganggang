# ganggang — long-horizon build task

**Implements: `design.html` v2** (the spec). This file decomposes it into ordered, commit-sized steps designed to be executed across many work sessions — by a human, a coding agent, or both. The spec is ground truth: if this file and the spec disagree, fix this file.

---

## 0. How to work this file (read every session)

**Start ritual:**
1. Read §2 (State) — it says exactly where the build is.
2. Find the first unchecked step. Read its spec section (linked as `§N`).
3. Skim `git log --oneline -5` to re-ground in recent work.

**Per step:**
1. Implement only that step's scope. No drive-by refactors, no scope creep.
2. Verify (§1 commands). A step is never done with a red test or a clippy warning.
3. Check the step's box, update §2 state, commit: `M2.3: bash tool` (step id in every commit message).

**End ritual:** commit everything (clean tree = resumable state). If mid-step at session end, record exactly where in §2 so the next session resumes cold.

**If blocked:** record it under §4 Blockers with what was tried, then move to the next unblocked step. Never silently skip.

**If a step reveals the spec is wrong:** stop, update `design.html` (with a changelog note), then continue. The spec changes only deliberately.

---

## 1. Ground rules

- **Commands** (all must pass before every commit):
  ```sh
  cargo fmt --all
  cargo test
  cargo clippy --all-targets -- -D warnings
  ```
- **Live gates** (a running Strata): `scripts/smoke.sh` for print mode, `scripts/tui-live.sh` for the TUI's terminal hand-off (through tmux; needs `cargo build --release`) — before a milestone or a TUI change lands.
- **Backend:** Strata at `http://127.0.0.1:8080/v1` (check `curl -s 127.0.0.1:8080/health`; if down, `strata launch`). Model `qwen3.8-flash-next-iq3_s`.
- **Style:** spec §18 — standard OSS Rust. `//!`/`///` docs, no tutorial comments, no `unwrap` outside tests.
- **Budgets:** modules stay ≲300 lines; split when exceeded.
- **Tests:** colocated `#[cfg(test)]`, fixtures in `fixtures/`, golden files where a wire/session format is touched.
- **Error policy:** spec §17 — tool failures are values fed back to the model; transport/IO aborts the run with context.

---

## 2. State  ← update every commit

| field | value |
|---|---|
| **Current phase** | **DONE** — M2–M8 complete |
| **Next step** | — (all TASKS.md items complete; §5 lists deliberate non-scope) |
| **Spec version** | design.html **v2.2** (hooks amendment, §22) + M2–M7 amendment log (see the changelog notes in design.html) |
| **Repo** | **Build complete**: fmt/clippy clean, smoke PASS (4 steps) against live Strata; every milestone gate closed by an independent fresh review with all findings fixed. Binary: `target/debug/gg` (release build not yet benchmarked). README.md written. `worktree-goal-finish-tasks` landed onto `main` (ff) 2026-11-23. Post-build features land as their own commits on `main` (including multi-provider support: `strata`, `glm-coding-plan`, `opencode-go`, `deepseek`, Anthropic Messages wire adapter, `--provider`/`--model`/`--list-models`, and TUI `/model`); hooks (spec + implementation) are rebased onto this `main` on branch `hooks-spec` — merge pending |
| **Notes** | M2 review produced: write `..`-traversal guard (lexical resolve), abort synthesizes tool_results for unanswered calls, bash kills its whole process group on timeout (`/bin/kill`, no unsafe per §01), Ctrl-C re-arms with second-press exit 130, capped reads everywhere (read +1-probe with UTF-8 tail back-off, edit refuses >50 KiB, bash drains past the 10 KiB cap), `RunAborted` terminal event, empty-answer Warned; config allowlist resolution lives in `config::resolve_tools` (CLI > config, validated against the registry); M4 gate fixes: run() owns the abort-flag reset, abort also checked after a tool batch, torn-tail newline repair + dangling-tool_call repair on resume, lazy session-file creation, second-precision stamps with mtime tiebreak, shared `tools::bash::run_bash` (bounded + group kill) for the tool and `!` shell, bracketed paste + row-wrapped transcript + panic-safe agent task, transcript/usage seeding on resume, `/context` + footer ctx%, osc52 char-boundary cap, tui split into mod/commands/agent_task/shell/clipboard/transcript{,/render}; post-build (2026-11-23): slash-command suggestion popup while the composer is a bare command word — pure `tui/suggest.rs` catalog (built-ins + prompt templates + per-skill `/skill:` rows, 9-row cap), ↑/↓ highlight + Tab completion, floats above the editor (§15 post-build amendment in design.html); its fresh review's fixes: selection clamps to the rows the terminal drew (render publishes the count), whitespace-named skills are not advertised (untypeable + unrunnable — classify splits at the space), `skill:`-prefixed prompt-template names refused at load, popup row builders moved into suggest.rs (§01 module budget); post-build (2026-11-23): Ctrl+G external editor — `tui/external.rs` (0600 draft in the temp dir, `$VISUAL` > `$EDITOR` > `nano`, saved text replaces the composer on exit 0, a failed editor keeps the composer and an edited draft) + `tui/reader.rs` (the key reader parks while the terminal is lent — a blocking read cannot be interrupted and two readers race on one tty), the loop keeps folding agent events behind the editor, repaint via `Terminal::resize` not `clear` (`clear`'s cursor-position query blocks 2 s and failed the session under a raw pty that answers nothing), no input flush on return (nvim's parting device-attributes query is consumed by crossterm itself; a flush dropped keys typed as the editor closed); `UiEvent` folding moved to `agent_task::apply` so mod.rs is back under the §01 budget; its fresh review's fixes: the tty mode is recorded once before gg first goes raw (`stty -g`) and put back on both return paths — after an editor and on the loop's own exit with one still open — because crossterm keeps one mode-before-raw and otherwise remembered a killed editor's raw mode, so quitting gg left the shell a raw terminal; the modes a killed editor leaves on are reset (mouse, focus, keyboard flags at every level, synchronized output, cursor keys, keypad, key modification — the last three found by a second review that confirmed the rest); keys sharing Ctrl+G's tty read are dropped before the key goes on (they cannot reach the editor; an Enter among them sent the draft sight unseen); an unreadable edited draft is kept and said to be kept; a loop exit with an editor open ends the editor first; an instant no-op return leaves a wait-flag hint; exactly one trailing newline and a BOM are dropped; drafts live in one private directory per run whose predictable name is taken only if it is the user's own (`tui/external/{command,draft}.rs` split for the §01 budget); known corner: non-DA terminal replies arriving after the editor exits garble input (no measured editor sends one); `scripts/tui-live.sh` is the live gate for all of it — each case on a tmux server of its own, since tmux options are server-wide in 3.8, including a killed nvim with `extended-keys` off and on (a third review's findings: a panic with the editor open left the tty raw too, now covered by the hook) (§15 post-build amendment in design.html); post-build (2026-11-23): `gg --watch` — the self-dev loop: mtime snapshot of `src/`+manifests (no dependency), debounced `cargo build` in a tokio task, then `exec` of the rebuilt binary on the same session after the terminal guard restores the tty; profile inferred from the running exe, `cargo metadata` refuses workspaces without a `gg` bin; restarts wait for idle and an empty composer or hold at `rebuild ready · Ctrl+R`, with Ctrl+R saving/restoring the draft beside the session; watch sessions materialize their JSONL at boot so `--resume` has something to find (`SessionStore::materialize`); failures show cargo's tail and keep the old binary; modules `tui/watch.rs` + `tui/watch/{config,restart}.rs`, live gate `scripts/watch-live.sh`; review fixes: stale-build discard/rebuild, `*.rs`-only snapshot (no swap files, no symlinks), `kill_on_drop` cargo, line-by-line warnings, header-only sessions sort below conversations but stay resolvable (`read_header` reports entries), draft chmod 0600, discovery requires the `ganggang` package (§15 post-build amendment in design.html); post-build (2026-11-23): hooks (§22, M8) — `src/agent/hooks/` (+24 unit tests): user-level loader (`~/.gg/hooks.toml`; project file ignored with a warning; `--no-hooks`), strict validation, typed JSON payloads (`session_id` via new `SessionStore::id`), bounded 64/8 KiB stdout/stderr with drain, 2 KiB reason cap, `bash -c` process group + `kill_on_drop` + SIGKILL on timeout, 100 ms abort polling; loop wiring (pre deny → persisted `is_error` result and post hooks skipped, post after the recorded result, run_end before `RunEnded` on clean finishes, `args_error` skips hooks, interrupted hooks synthesize `aborted before execution`; 13 fake-server tests); `--no-hooks` CLI; TUI `/hooks` (catalog cap 11 in the merged built-in set); README Hooks section + security stance; live gate `scripts/hooks-live.sh` PASS (deny leaves no side effect, post/run_end markers, `--no-hooks`, project file ignored); review fixes: absolute-only HOME across user paths, args_error skips post hooks, Drop guard group-kills on external cancellation, stderr tail/invalid-output stderr, canonical tool names in agent::tools; 367 tests, fmt/clippy clean, smoke PASS |

---

## 3. Phases and steps

### Phase M2 — agent core, tools, print mode  (spec §7–§10, §16, §15-print)

- [x] **M2.1 — `AgentEvent` enum** · `src/events.rs` (spec §7.3)
  The event enum exactly as specced: `RunStarted, TurnStarted, AssistantText, ToolCallStarted, ToolCallFinished, TurnEnded, RunEnded, Warned`, `#[serde(tag="type", rename_all="snake_case")]`, `Clone + Send`.
  Tests: each variant serializes to the expected one-line JSON shape (`{"type":"turn_started",...}`).
  Done when: compiles standalone with tests green; nothing else uses it yet.

- [x] **M2.2 — domain session types** · `src/agent/session.rs` (spec §7.2, §13)
  `Entry` (`Message` \| `Compaction`, serde-tagged `type`, `id`/`parentId`/`timestamp`), `Message` with `Role` enum (`user|assistant|tool_result` snake_case), parsed-JSON tool args (`Value`), `usage`, `is_error`. Entry ids: 8-hex from `uuid` v4. Timestamps: ISO-8601 via `time`.
  Tests: round-trip serde against hand-written JSONL fixture; role serialization is snake_case.
  Done when: `fixtures/session_minimal.jsonl` round-trips `Entry`-by-`Entry`.

- [x] **M2.3 — domain ↔ wire conversion** · `src/agent/session.rs` (spec §7.2)
  `to_wire(&[Entry]) -> Vec<ChatMessage>` and `from_response(ChatResponse) -> Message` (captures tool calls with parsed args + `args_error`, usage; string-arguments parse failures become a `Warned` + empty args + `args_error` on the call — dispatch returns the §10 error result, and the call replays with `{}`, never dropped).
  Tests: tool-call fixture response → domain → wire → replays identically to the M1 round-trip test; usage carried through.
  Done when: the M1 test `tool_call_message_round_trips_for_replay` passes against the new path.

- [x] **M2.4 — request `tools` field** · `src/llm/types.rs` (spec §7.1)
  `ChatRequest` gains optional `tools: Vec<Value>` (`skip_serializing_if` empty). Schema literal for a tool lives with the tool (M2.6+), not here.
  Tests: request with tools serializes the OpenAI function-declaration shape; without tools, key omitted.

- [x] **M2.5 — `Tool` trait, registry, dispatch** · `src/agent/tools/mod.rs` (spec §10)
  `ToolResult { output, is_error }`, `trait Tool` with manual `BoxFuture` (no async-trait crate), `Registry(Vec<Box<dyn Tool>>)`. Dispatch: `args_error` (set by `from_response`'s boundary parse) ⇒ error result without invoking the tool; unknown tool ⇒ error result; per-tool args structs deserialize permissively. Never a run abort.
  Tests: dispatch with a stub tool — good args, missing-field args, `args_error` args, unknown tool name; all produce results, none panic/abort.

- [x] **M2.6 — `read` tool** · `src/agent/tools/read.rs` (spec §10 table)
  Args `path`, optional `offset`/`limit`. Caps 2000 lines / 50 KB, truncation noted in output. Missing file ⇒ error result.
  Tests: tempdir read, truncated read, missing file.

- [x] **M2.7 — `bash` tool** · `src/agent/tools/bash.rs`
  Args `command`, optional `timeout_secs` (≤ configured max). `tokio::process`, capture stdout+stderr merged, report exit code, truncate output at 10 KB, kill on timeout.
  Tests: `echo`, `false` (nonzero exit → error result with output), short timeout kill.

- [x] **M2.8 — `edit` + `write` tools** · `src/agent/tools/edit.rs`, `write.rs`
  edit: exact-match replacement, each oldText unique & non-overlapping; miss/duplicate ⇒ error result, file untouched. write: create/overwrite, refuse paths outside cwd unless `force`.
  Tests: happy path, no-match, ambiguous-match, outside-cwd refusal; write create + overwrite + refusal.

- [x] **M2.9 — `Agent` + loop** · `src/agent/mod.rs`, `src/agent/loop_.rs` (spec §9 — note: **no turn limit**)
  `Agent { client, session, tools, cfg, abort }` with `run(input, &mut Sender<AgentEvent>)`: append user → build request → send → append assistant → dispatch tool calls sequentially → repeat until no tool calls. Abort flag (`Arc<AtomicBool>`) checked between tool calls; abort appends marker + returns Ok. Delete the M1 manual loop from `main.rs` here.
  Tests (fake server on std `TcpListener`, scripted responses): plain answer (1 turn, RunEnded); tool-call→result→answer (2 turns); tool error recovery; malformed tool arguments (assert ToolCallFinished `is_error` with "not valid JSON", a `Warned` fired, run continued); abort mid-batch.

- [x] **M2.10 — config v2** · `src/config.rs`, `Cargo.toml` (spec §16)
  Add `toml` crate. Load order CLI > env > `.gg/config.toml` > `~/.gg/config.toml` > defaults. New defaults: `max_tokens 4096`, `context_window 131072`, `api_key` (Bearer header when set), `bash_timeout_secs`, `tools` allowlist. Remove nothing that exists. **No `max_turns`.**
  Tests: `parse_env`-style pure fns for TOML merge (invalid values are errors, not defaults); env overrides file, file overrides default.

- [x] **M2.11 — print mode** · `src/printers.rs`, `src/main.rs` (spec §15)
  `gg -p "…"` (+ stdin piping into the prompt): progress to stderr (turn markers, one line per tool call), final assistant text to stdout. Plumb `--tools` flag to the registry.
  Tests: printer unit test consuming a scripted event vec; fake-server end-to-end through `Agent::run`.
  Done when (milestone acceptance): `cargo build && ./target/debug/gg -p "what version is in Cargo.toml?"` answers 0.1.0 **via the read tool against live Strata**.

- [x] **M2.12 — M2 gate** · review
  Run `scripts/smoke.sh` (already written: health check + the Cargo.toml question + a bash-tool edit round-trip). Run the M2 review with a fresh reviewer prompt (`.review-m2-prompt.md`). Fix findings. Update State → phase M3.
  Ran: smoke PASS; independent reviewer found 5 bugs + 2 spec decisions + 4 idiom fixes — all fixed. Spec → v2.1 (`RunAborted`, abort tool_results, empty-content Warned; see design.html changelog).

### Phase M3 — context assembly + skills  (spec §11, §12)

- [x] **M3.1 — preamble + section assembly** · `src/agent/context.rs` (spec §12)
  `const PREAMBLE` (~40 lines: identity, cwd, tool discipline, edit rules, brevity) + `build_request` assembling preamble → context_files → tools → skills → working_dir sections.
  Tests: section order + content snapshot with no context files/skills present.
  Built as `build_system_prompt(cwd, tools, agents_files)`; the loop prepends it as the system message per request. Skills section lands in M3.5 (parameter omitted until then). Tool lines derive arg hints from each tool's JSON Schema.
- [x] **M3.2 — AGENTS.md discovery** · `src/agent/context.rs`
  `~/.gg/AGENTS.md` then `AGENTS.md` from repo root (git root or fs root) → cwd, concatenated. Missing = absent section.
  Tests: tempdir tree with two AGENTS.md files → both included, root first; none → omitted.
  Discovery runs once at `Agent::new`; origins are repo-relative labels. Innermost `.git` wins; no `.git` anywhere falls back to the fs root (spec's stated rule).
- [x] **M3.3 — skill frontmatter parser** · `src/agent/skills.rs` (spec §11)
  Hand-parsed `---` fences: `name`, `description`, `disable-model-invocation`. Unknown keys ignored. No frontmatter / missing description ⇒ skipped skill.
  Tests: happy path, unknown keys, missing description, no fences, `disable-model-invocation`.
  Name is authoritative from frontmatter (dir name ignored); `disable-model-invocation` rides on `Skill` for M3.5's advertiser to filter.
- [x] **M3.4 — skill discovery** · `src/agent/skills.rs`
  Scan `~/.agents/skills/` then `.agents/skills/` walking cwd→ancestors (stop at repo root). Directory+`SKILL.md` only. First-wins collisions.
  Tests: fixture dirs — order, collision, nesting depth, `.agents` in ancestor.
  Only immediate child dirs of `skills/`; scan sorted for deterministic first-wins; `discover_with_roots` keeps tests env-free.
- [x] **M3.5 — advertising + `GG_DEBUG_PROMPT`** · `context.rs`
  Skills section: name + description + absolute path + load instructions. `GG_DEBUG_PROMPT=1` dumps the assembled system prompt to stderr.
  Tests: advertised list matches fixtures; scripted fake-server run shows the skill path in the outgoing request body.
  Done when (acceptance): a fixture skill's instructions are followed in a scripted run; `GG_DEBUG_PROMPT=1 gg -p hi` shows the full prompt.
  Acceptance ran live: `.agents/skills/codeword` advertised, read via the read tool, followed exactly; prompt dump verified (preamble → Tools → Skills → Working directory).
- [x] **M3.6 — M3 gate** — smoke + review template + State → M4.
  Ran: smoke PASS (incl. new skill step 4); independent reviewer found 3 bugs (CRLF fence panic, home-skills rescan, quote retention), 3 decisions to record, 1 dup, stale allows, 6 test gaps — all fixed across three commits (`M3 gate fixes` 6ecd496 / b633719 + uncommitted-at-the-time test pins swept into M4.1). Spec changelog records the lenient skill-name + BOM + quote decisions.

### Phase M4 — session persistence + TUI  (spec §13, §15)

- [x] **M4.1 — session file I/O** · `src/agent/session.rs` (`session/io.rs`)
  Append-only JSONL under `~/.gg/sessions/<path-slug>/`; header line first; one line per entry, flush per append; load skips a corrupt trailing line with a warning. In-memory sessions for `--no-session`.
  Tests: append/load round-trip golden file; corrupt-tail tolerance; header written once.
  `SessionStore::create_under` keeps tests HOME-free; discovery split into pure cores (`latest_in`, `resolve_prefix`) + HOME wrappers; corrupt lines ANYWHERE skip-with-warning (mid-file has no better recovery — documented); version ≠ 1 files are rejected, not mis-parsed; open_existing refuses to extend a corrupt file. Golden: `fixtures/session_golden.jsonl` (pins byte-exact field order). The loop's push fns are now Agent methods that persist before mutating memory (§13 flush-per-append, §17 session-IO aborts).
- [x] **M4.2 — resume CLI** · `src/main.rs`
  `gg -c` (newest session for cwd), `gg --resume <id|prefix>` (error if ambiguous), `--no-session`. Every mode persists by default.
  Tests: create → `-c` picks it up → `to_wire` of loaded == in-memory.
  Prefix matches the session uuid OR the 8-hex filename id (the handle users see in the `session:` line); clap enforces the flag conflicts (tested); `open_session` replays loaded entries + load warnings to stderr. Fixed live-caught bug: `create` double-slugged the path (regression test pins create→discover composition). Verified live against Strata: `-c` and `--resume <hex>` both recall prior context.
- [x] **M4.3 — TUI shell** · `src/tui/mod.rs`, `view.rs`, `input.rs`, `footer.rs`, `term.rs` (ratatui 0.30 + crossterm 0.29)
  Fullscreen transcript/editor/footer; agent runs on a tokio task, events over mpsc; footer shows cwd, model, token totals. Enter send · Shift+Enter newline · Esc abort (sets the M2.9 flag).
  Tests: none practical for the terminal loop — verified by M4.6 manual pass. The pure parts are tested: key classification (Enter/Shift+Enter/Ctrl+J newline, Ctrl+C quit, Ctrl+O expand, Esc abort, PageUp/Down scroll, release events ignored), editor char-wise editing, usage totals (assistant entries only), view folding + bottom-anchored scroll.
  Design: agent task owns the Agent for the whole session; UI ↔ agent via input channel + `UiEvent` (Agent/RunComplete-with-usage/RunFailed). Shift+Enter arrives only from modifier-reporting terminals — Ctrl+J is the documented portable newline. Key reader on a dedicated thread (crossterm read blocks). Terminal guard + panic hook restore raw mode/alt screen. `gg` on a TTY → TUI; piped stdin without -p is a named error. Usage totals sum assistant entries only (a provider stamping usage on user messages must not inflate them).
- [x] **M4.4 — transcript rendering** · `src/tui/transcript.rs`
  User text, assistant text, tool calls as collapsed `▸ name args…` (Ctrl+O expand), `Warned` dimmed, abort marker.
  Pure model (`Transcript::apply` folds events into blocks; `render` emits styled-kind lines — no terminal dependency). Collapsed lines carry ✓/✗ once finished; expanded output caps at 30 lines with a `[truncated: N lines]` marker; args one-line to ~60 chars with ellipsis. Ctrl+O expands the most recent collapsed call first; when all are expanded it collapses all (documented rule). Deltas append within a turn and split across turns. 8 model tests; view fold/scroll tests updated to the full model.
- [x] **M4.5 — commands + `!` shell** · `src/tui/mod.rs`
  `/new /resume /skills /session /quit /skill:<name> [args]` (skill load ⇒ instructions appended as user message); `!cmd` runs shell, output enters context; `!!` doesn't. Ctrl+X copies last answer.
  Done when (acceptance): interactive session survives restart via `-c`; Esc mid-tool-batch leaves a loadable file.
  Live tmux acceptance (all verified): prompt→answer; /skills (home skills + paths); /session (path/entries/tokens); !echo → shell block + model acknowledges context; Esc mid-batch → ⚠+■ marker, clean ready; /resume listing (timestamp+uuid, `/resume <n|prefix>` pick); /skill:codeword pineapple → PINEAPPLE-77 exactly; Ctrl+X → "copied 12 chars" (OSC 52, zero deps); /quit clean. `/new` + `/resume` rebuild the agent through a factory closure (main owns construction); corrupt files are refused for append by design (M4.1).
- [x] **M4.6 — M4 gate** — manual TUI pass (abort, resume, crash-kill test: `kill -9` mid-run → file loads), smoke, review, State → M5.
  Review ran first (prompt `4386be6`): 5 fix commits (abort lifecycle, crash-safety, TUI robustness, resume seeding/ctx%, smoke assertion). Live pass (worktree binary, tmux): prompt→answer with usage footer; Esc mid-tool-batch → ⚠ + ■ markers, ✓ on the completed call, clean ready; `/session` path+entries+tokens; `/quit` clean; `kill -9` mid-tool-call → session file has dangling tool_call, loads via `-c` with "repaired 1 interrupted tool call(s)" warning, model recalls both crash-run keywords (print + TUI `-c`, usage seeded). Smoke PASS (4 steps incl. skill).

### Phase M5 — streaming  (spec §15, §7.1)

- [x] **M5.1 — SSE client path** · `src/llm/client.rs`
  `stream=true` request; parse `data:` lines (`[DONE]` sentinel); accumulate text deltas + tool-call fragments; non-streaming path retained for tests.
  Tests: fake SSE server feeding chunked fixtures → assembled `ChatResponse` equals the non-streaming fixture for the same conversation.
  Landed as `src/llm/stream.rs` (+tests): byte-level `LineBuffer` (CRLF/multibyte-safe), `Assembler` folding index-keyed tool-call fragments, `drain_sse` delivering deltas through a boxed-future callback. Strata probed live: honors `stream_options.include_usage` (usage on a `choices: []` final chunk), `: keep-alive` comments skipped, `reasoning_content` deltas ignored (M7.3), non-standard `timings` tolerated. Agent seam: `Agent { stream: bool }`, true in production, false in scripted loop tests.
- [x] **M5.2 — delta events + live render** · `events.rs`, `tui/`, `printers.rs`
  `AssistantText { delta }` emitted per chunk; TUI appends live; print mode shows a spinner/tick to stderr only.
  Done when: tokens appear as generated in the TUI against live Strata.
  Acceptance verified live: transcript grows across 300ms-pane captures mid-generation; usage recorded from streams (footer updates); stdout pipe-clean (`od`/`wc` verified by reviewer + orchestrator). TUI needed no transcript changes — the M4.4 fold already appends deltas within a turn.
- [x] **M5.3 — M5 gate** — smoke, review, State → M6.
  Independent reviewer (fresh context, live Strata): acceptance verified live (incremental TUI rendering across 300ms captures, pipe-clean stdout, streamed tool-call replay, Esc mid-stream, usage honored); 4 findings — no-DONE EOF silently accepted (§17), role-echo `""` content recorded on tool-call turns (shape divergence), spinner residue on failed runs, spinner ANSI on non-TTY stderr — all fixed in `bd16649` + live-verified; spec M5 amendments recorded in design.html. Test gaps closed in two batches (no-DONE socket test, role-echo shape test, malformed-chunk + id/name-continuation pins, fake server now emits Strata's real `content: ""` echo so the parity test guards the regression). Printers.rs 332-line nit accepted (145 non-test lines; budget read on implementation size).
- [x] **M6.1 — token accounting + trigger** · `src/agent/compaction.rs`
  Track context tokens from last `usage` (chars/4 fallback); after each tool batch: trigger when `tokens > context_window − 16384`.
  Tests: pure trigger fn table test; integration: tiny `context_window` forces compaction point.
  `context_tokens_of` = last assistant usage (prompt+completion); `estimate_wire_tokens` = chars/4 of the wire request only when usage absent; `should_compact` with `saturating_sub` (window < reserve still triggers). Per-entry walk estimates use the entry's own text mass — usage numbers would double-count each turn. `Agent` gains `context_window` plumbed from config. Tiny-window integration test lives with the M6.2 loop hook where the action is.
- [x] **M6.2 — summarize + rebuild** · `compaction.rs`, `session.rs`
  Cut at `keep_recent 20000` snapped to user-message boundary; one summarize LLM call (failure ⇒ Warned + continue un-compacted); append `Compaction` entry; `to_wire` emits summary + entries from `firstKeptId`.
  Tests: golden before/after context build; summary-failure fallback; repeated compaction starts at previous kept boundary.
  Walk covers only the ACTIVE region (from the previous compaction's boundary) so re-compaction never re-summarizes dead history; snap is backward-first, forward when the walk slides past a boundary onto a zero-mass tail; dropped region for the summarize call is the current replay view minus the kept suffix (carries the old summary forward — raw slicing lost it); summarize uses the retained non-streaming client path; successful compaction reports as a `Warned` (§17 visibility, no dedicated event variant).
- [x] **M6.3 — `/compact` + JSON mode** · `src/tui/mod.rs`, `src/printers.rs`
  `/compact [instructions]`; `gg --mode json` — session header record then one `AgentEvent` per line to stdout (stderr stays for diagnostics).
  Tests: golden event-stream file for a scripted run.
  Done when: long scripted run compacts and continues correctly; `--mode json` output parses line-by-line with `jq`.
  JSON mode landed as `src/json_mode.rs` (session header via `SessionStore::header_record`, omitted for `--no-session`); `/compact` shares `compact_once` with the loop hook. Live acceptance (fork + orchestrator): `jq -e` parses every event line; three 50KB-read runs under `GG_CONTEXT_WINDOW=6000` compact mid-run and continue; TUI `/compact keep the keywords…` on a 58k-token session → "compacted: 18 entries summarized", ctx 44%→30%, post-compaction recall of dropped-region keywords works through the summary; "nothing to compact yet" on sub-20k sessions confirmed correct (keep budget not exceeded).
- [x] **M6.4 — M6 gate** — smoke, review, State → M7.
  Independent reviewer: "spec-conformant and live-verified end to end" (auto-compaction in print/JSON/resume chains, repeated compaction carries the first summary, `/compact` focus honored, boundaries/jq verified). 6 findings — trigger-true/no-cutpoint silence (§17), broken pipe as task panic, chars/4 CJK drift (documented), swallowed header-record error, overcounted "N entries", stale footer ctx% — all fixed (`c5d1078` + remainder batch). Remainder: stale allows swept, forward-snap + persistence round-trip + no-cutpoint-warning + `/compact` plumbing tests pinned, `/compact`-while-busy now says so (recorded). Test gaps left deliberately: summarize `Focus:` formatting and JSON-mode stdout-shape e2e are live-verified only. Spec changelog records the M6 amendments.

### Phase M7 — polish (spec §2 later-rows)

- [x] **M7.1 — `grep` tool** (content search: pattern, path, output caps) + tests
  Fixed-substring (no new dep — regex is only transitive; description points at bash for regex). `.git` pruned, binaries skipped, 50-match cap + notice, 250-char line cap, 1 MiB/file read cap. `eccbbbe`; default-allowlist amendment recorded in design.html.
- [x] **M7.2 — `find` + `ls` tools** + tests
  find: `*`/`?` name glob, recursive, sorted, cap + notice, dotfiles included. ls: one level, dirs-first alphabetical, size, unreadable entries noted not failed. `46b7728`; config test pins every registered tool is in the default allowlist.
- [x] **M7.3 — thinking display** — capture `reasoning_content` (wire type already permissive), render hidden behind Ctrl+T in TUI, omitted in print mode
  `3fbbfd2`: `Delta::{Text,Thinking}` SSE callback, `Message.reasoning` persisted-when-non-empty and never replayed to the model (pinned), new `Thinking { delta }` event (amendment recorded), Ctrl+T toggles dim `~ `-prefixed blocks (30-line cap), resume seeds persisted thinking, print mode renders nothing and keeps the spinner alive. keys.rs + transcript/seed.rs split keeps modules under budget. Live: 19 thinking events on a `--mode json` run; TUI Ctrl+T verified both directions with the model's real reasoning.
- [x] **M7.4 — prompt templates** — `~/.gg/prompts/*.md` frontmatter (`description`) → `/name` commands in TUI
  `6ac1dee`: `src/prompts.rs` loader (no-fences valid with stem name, built-ins win collisions, first-name-wins, invalid files skip with startup warnings); `/name [args]` INSERTS the draft into the editor (Enter submits); `/prompts` lists. `Command::Unknown { name, args }` carries the unknown-command shape. Amendments recorded (`a614f45`). Live: `/prompts` lists with path+description; `/review Cargo.toml` drops body+args into the editor.
- [x] **M7.5 — `@file` mentions** — expand `@path` in editor input into attached file content (text) 
  `f351551`: `src/tui/mentions.rs` — "looks like a path" rule (`/` or `.` after `@`; emails/lone `@` literal), 50 KB visible cap, binary/missing/cap markers, dedupe, transcript shows the typed line. Punctuation-trim fix (`@file?` mid-sentence) on top. Live: model quoted an attached `version = "9.9.9"` verbatim with zero tools; missing file degrades to a note the model cites honestly. Print mode stays literal (editor behavior, recorded).
- [x] **M7.6 — markdown rendering** in TUI transcript (minimal: code blocks, bold, lists)
  `e9b723a`: `transcript/markdown.rs` — fences → `LineKind::Code` (fence lines drop, unterminated fence = code until closed, streaming-safe via per-frame re-render), `**` pairs bold char-ranges (wrap-split spans bold both rows at correct offsets), bullets → `•` with indent, everything else literal; user text untouched. Live: bullets/code/bold all rendered with zero literal markers.
- [x] **M7.7 — session export → HTML** — `gg --export <session> [out.html]`, single self-contained file (like `design.html`)
  `babae9a`: `src/export.rs` — path/id/prefix input (resume's resolution helpers), two-pass render splicing tool results into their calls' `<details>`, thinking collapsed, compaction markers, per-entry usage, `esc()` on every content kind, zero `<script>`/external refs (pinned with hostile payloads). Live: 164 KB export of the compaction-bearing m6 session, self-containment grep-clean.
- [x] **M7.8 — final gate** — full smoke, review, README written (security stance verbatim from spec §10 note), State → **DONE**
  Final independent reviewer: "M7 passes: everything TASKS.md §3 promised is implemented, spec-conformant, and live-verified end to end; I'd daily-drive it. No security holes." Export escaping verified hostile-payload-clean through every content kind; thinking-never-replayed pin holds. 6 robustness findings — exponential find glob (could wedge a run), mention whole-file line count, symlinked-dir roots refused by grep/find/ls, whitespace template names unloadable, dead files_searched counter, two trivia — ALL fixed with pins (iterative two-pointer matcher + pathological-pattern test, streamed BufRead count on truncation only, metadata-follows-root, load-time refusal, scoped "(no matches in N files)", real read errors). Remaining known gaps, recorded: tui/commands.rs 315 lines (a hair past guidance), summarize Focus-formatting + JSON stdout-shape e2e live-verified only. Late test-gap tail also fixed: mention dedupe by resolved path (`@a.txt` vs `@./a.txt` attaches once), binary re-check past the 8 KiB sniff; export full-document golden left unpinned deliberately (escaping is the property that matters and is pinned). 250 tests, gates green, smoke PASS.

### Phase M8 — hooks  (spec §22; §02–§04, §07, §09, §10, §13, §15–§17, §19)

- [x] **M8.1 — hook config + runner** · `src/agent/hooks.rs`, `src/agent/session/io.rs` (spec §22)
  `~/.gg/hooks.toml`: `[[hooks]]` with `event` (`pre_tool`/`post_tool`/`run_end`), `tools` (exact names or `["*"]`/omitted; tool events only), `command` (non-empty), `timeout_secs` (default 10, positive). Load once at startup; user-level only — a project `.gg/hooks.toml` is ignored with a startup warning; `--no-hooks` skips load. `SessionStore::id()` for the payload. One-shot `bash -c` runner: JSON stdin, stdin closed, stdout/stderr bounded (64/8 KiB) and drained, own process group + `kill_on_drop` + group SIGKILL on timeout, abort polled every 100 ms. Runner returns values (allow/deny/warnings/interrupted); only the loop emits events.
  Tests: golden payload per event; config validation (unknown event/tool, empty tools, `tools` on `run_end`, zero timeout, missing command, malformed file); missing file = no hooks; project file warning; allow / explicit allow / deny / unknown decision / invalid JSON / non-zero / timeout / spawn failure; caps and char-boundary truncation; a call with `args_error` skips hooks.
  Landed: `src/agent/hooks.rs` + `hooks/{invoke,payload,report,run}.rs` + `hooks/tests.rs` (24 tests; each production module under the §01 budget). `Hook`/`HookEvent`/`Hooks`, typed payloads (`version`/`event`/`cwd`/`session_id`/`tool`/`result`/`turns`), reason and stderr caps, `SessionStore::id`. Defaults/validation per spec; unknown event is a TOML parse error, everything else validates with the file path in the message. `Hooks::new` is test-only.
- [x] **M8.2 — loop integration** · `src/agent/loop_.rs` (spec §22, §09)
  `pre_tool` after `ToolCallStarted`, before dispatch: a deny appends a persisted `is_error` result (`denied by pre_tool hook: <reason>`), emits `ToolCallFinished`, skips dispatch and post hooks; first deny wins; other calls in the batch proceed. `post_tool` after `ToolCallFinished` (result already recorded), observe only. `run_end` once before `RunEnded`, clean final answers only. Failures become `Warned` and never abort; an interrupted hook gives the call `"aborted before execution"` and the existing abort path finishes the batch.
  Tests (fake server): deny leaves no side effect and the model sees the reason; post/run_end marker files; hook failure warns and the run continues; first-deny-wins; aborted run fires no `run_end`; no hooks for compaction or the `!` shell.
  Landed: `loop_.rs` wiring (session id captured once per run, warnings before the corresponding decision/finish, post skipped on deny/interruption, `args_error` skipped for both pre and post). 13 `loop_/tests/hooks.rs` scripts: deny/no side effect + model-visible reason, post/run_end markers, no post on deny, fail-open, first-deny-wins, post failure cannot change the result, args-error skips both hook kinds, abort fires no run_end, session id in payload, warnings before `RunEnded`, abort during a pre hook, one denied call leaves the rest of the batch running, transport failure skips run_end.
- [x] **M8.3 — interface + docs** · `src/tui/{commands,suggest}.rs`, `src/main.rs`, `README.md` (spec §15, §16, §22)
  `/hooks` lists loaded hooks (event, tools, timeout, command) or says disabled/empty; add to the suggestion catalog and `known_commands`. Startup project-hook warning reaches stderr in TUI, print and JSON modes. README: Hooks section (file, events, protocol, fail-open, `--no-hooks`), command list, and the security stance sentence. No new `AgentEvent` variants; JSON/print shapes unchanged.
  Landed: `--no-hooks` on the CLI, `Hooks::load` in both boot paths (TUI + print/JSON) with stderr warnings, `Discovered.hooks`/`CmdCtx.hooks`, `/hooks` handler + classification + `known_commands`, suggestion row and cap 11 (nine → ten for `/hooks`, then eleven once `/model` merged; design changelog amended), README Hooks/security/dev-gate. No `AgentEvent` change.
- [x] **M8.4 — M8 gate** — `scripts/hooks-live.sh` (temp HOME + workspace against Strata: a `pre_tool` deny prevents a real side effect and the answer says why; `post_tool`/`run_end` markers fire; a failing hook does not stop the run; project hooks are ignored), `cargo fmt/test/clippy`, smoke, fresh review, State → **DONE + M8**.
  Landed: `scripts/hooks-live.sh` PASS against live Strata — deny left `pwned` absent and the answer quoted the hook reason; post-read and run-end markers fired; a failing `post_tool` hook warned and the run still answered; `--no-hooks` let the same bash call create the file and suppressed run_end; a project `.gg/hooks.toml` produced the warning and no marker. 367 tests, fmt/clippy clean, smoke PASS (4 steps).
  Fresh-review fixes: an `args_error` call no longer runs a post hook (the `executed` flag gates both hooks; marker test added); `HOME` must be absolute for every user-level path (`src/home.rs`; relative/empty is "no home", so a repo cannot supply hooks/config/skills/sessions/prompts/AGENTS); a `Drop` guard group-kills a hook on external cancellation (tested by dropping the future around a background child); stderr keeps its **last** 8 KiB and invalid-output warnings include it; the canonical tool-name list lives in `agent::tools::NAMES` (config + hooks validate against it, registry test pins it). Test tail: pre-hook abort, multi-call one-deny, transport failure skips run_end, >pipe payload, >64 KiB stdout drain, stderr tail, dropped-future group kill.

---

## 4. Blockers

| step | what | tried | next |
|---|---|---|---|
| — | — | — | — |

---

## 5. After DONE (explicit non-scope, revisit deliberately)

Hooks landed as M8 (§22) — one-shot, user-level shell hooks at `pre_tool`/`post_tool`/`run_end`. Still deferred: the long-lived plugin runtime (custom tools/commands/TUI; out-of-process JSONL candidate — spec §03), project-scoped hooks (need a trust story), session tree navigation (`/tree`, fork), steering queue, retries, image content, MCP, RPC/SDK. Each needs a spec revision first, per §0 rules.
