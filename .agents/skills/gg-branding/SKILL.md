---
name: gg-branding
description: "Apply the ganggang (gg) brand and gang-gang cockatoo theme to TUI views, HTML, docs sites, diagrams, and assets."
---

# ganggang Brand Guide

## Identity and naming

- **Project and crate name:** `ganggang` (always lowercase; never `GangGang` or `Gang-Gang`).
- **Binary and short mark:** `gg` (lowercase; bold in scarlet crest `#D94B4B` when used as a brand mark).
- **Tagline:** `a small, sharp agentic harness` (from `src/tui/view/welcome.rs` and `Cargo.toml`).
- **Subject:** the Australian gang-gang cockatoo (*Callocephalon fimbriatum*): charcoal-grey plumage, scarlet crest, orange-red cheeks, warm cream beak, pale grey wings with a grey-green covert wash, paired with Australian wattle gold for warnings.
- **Visual character:** restrained, utilitarian, dark-first. Charcoal grounds and warm off-white text carry most of the screen. Scarlet is reserved for `gg` marks, active selections, and stop/abort states. Orange-red marks live activity.

## Canonical 16-role colour palette

Source of truth: `src/tui/theme.rs` (`TRUECOLOR`) and `src/tui/theme/fallback.rs` (`indexed_palette`).

| Role | Hex | RGB | xterm-256 | Bird reference | Semantic usage |
|---|---|---|---|---|---|
| `bg` | `#16191C` | `22, 25, 28` | `234` | Charcoal plumage | Full-screen canvas background |
| `outline` | `#000000` | `0, 0, 0` | `16` | Deep shadow | The black edge that lifts the scrollbar off the ground |
| `surface` | `#202429` | `32, 36, 41` | `235` | Raised plumage | Composer, footer, panels, cards, sidebars |
| `surface_alt` | `#2A2F35` | `42, 47, 53` | `236` | Raised slate | Code bands, expanded tool output, floating popups |
| `border` | `#3A4047` | `58, 64, 71` | `238` | Feather edge | Hairline rules, dividers, scrollbar track, idle box border |
| `selection` | `#3A2B2B` | `58, 43, 43` | `237` | Warm charcoal | Selected popup row and active list highlight |
| `text` | `#E9E3D7` | `233, 227, 215` | `254` | Warm cream beak | Primary body text and active model name |
| `muted` | `#98A0A6` | `152, 160, 166` | `247` | Pale grey wings | Secondary text, tool args, paths, token totals |
| `dim` | `#6B7379` | `107, 115, 121` | `243` | Shadow grey | Hints, placeholders, info lines, thinking, expanded output |
| `code` | `#C7D0C0` | `199, 208, 192` | `251` | Sage-tinted grey | Code block text on `surface_alt` |
| `accent` | `#D94B4B` | `217, 75, 75` | `167` | Scarlet crest | `gg` mark (and the flashing name throughout a stop), `> ` user mark, popup `▸`, scrollbar thumb, stopping border, `stopping…`, abort line, primary actions |
| `accent_soft` | `#E07A4F` | `224, 122, 79` | `173` | Orange-red cheek | Live states: tool calls, the working mark's flash (off the crest frame), busy composer border |
| `accent_deep` | `#8E3030` | `142, 48, 48` | `95` | Deep crest red | High-emphasis borders, active or pressed red accents |
| `ok` | `#A7B38F` | `167, 179, 143` | `144` | Grey-green wing wash | Success (`✓`), approved states, positive deltas |
| `warn` | `#D9A441` | `217, 164, 65` | `179` | Wattle gold | Warnings (`⚠`), watch-mode badge, context usage 61% to 85% |
| `error` | `#E06A6A` | `224, 106, 106` | `168` | Soft scarlet | Failed tool calls (`✗`), command errors, context usage above 85% |

### Palette invariants

- **Semantic roles only:** widgets, HTML templates, and stylesheets reference role names (`bg`, `surface`, `accent`, `muted`), not raw hex values scattered in logic.
- **Luminance order:** grounds step up (`bg < surface < surface_alt`); foregrounds step up (`dim < muted < text`).
- **Distinct 256-colour fallback:** all 16 roles map to distinct xterm-256 indices, claimed in priority order (marks and status first, then text and greys, then grounds).

## CSS custom properties and web typography

Use this `:root` block for standalone HTML outputs, reports, and web surfaces:

```css
:root {
  color-scheme: dark;
  --bg: #16191C;
  --outline: #000000;
  --surface: #202429;
  --surface-alt: #2A2F35;
  --border: #3A4047;
  --selection: #3A2B2B;
  --text: #E9E3D7;
  --muted: #98A0A6;
  --dim: #6B7379;
  --code: #C7D0C0;
  --accent: #D94B4B;
  --accent-soft: #E07A4F;
  --accent-deep: #8E3030;
  --ok: #A7B38F;
  --warn: #D9A441;
  --error: #E06A6A;
  --sans: ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
  --mono: ui-monospace, "SF Mono", "Cascadia Code", Menlo, Consolas, monospace;
}
```

### Standalone HTML rules

- Inline all CSS and any required JavaScript; include zero external fonts, CDNs, or network requests.
- Use native `<details>` and `<summary>` for collapsible sections where possible.
- Include responsive collapse (`@media (max-width: 900px)`) and legible `@media print` overrides.

## UI glyphs, separators, and state rules

Exact characters and rules from `src/tui/`:

- **Separators:** ` │ ` (U+2502) for footer fields; ` · ` (U+00B7) for inline hints and metadata.
- **Markers:** `> ` (`accent` bold: opens a user message once; the rows after it indent two columns), `▸` and `▾` (collapsed and expanded tool calls; `▸ ` in `accent` on `selection` for active popup rows), `$ ` (shell command), `✓` (`ok`), `✗` (`error`), `⚠ ` (`warn`), `~ ` (`dim` italic for thinking), `■ run aborted` (`accent` bold), `stopping…` (`accent` bold: the footer's one status word), `↑in ↓out` (token totals), `• ` (list bullets), `…` (ellipsis), `[truncated: N lines]`.
- **Mark:** two cells, `gg` bold in `accent` at rest. While a run is live the same two cells are the gang-gang's mark: the name itself flashing on, off, on (`gg `, `  `, `gg `), one frame per 600 ms (`crate::tui::tick::PERIOD`), `accent` on the crest frame (the first `gg `) and `accent_soft` the rest. The trailing cell stays blank, so the mark fills exactly the cells `gg` does and nothing behind the mark moves while a run turns. `accent` throughout when stopping. The footer draws the mark once, in those cells, and that is all it says about a run at rest or at work: no status word, no status field. `stopping…` is its one status field, in `accent` bold. Print mode's `streaming…` line (`src/printers.rs`) takes the same frames (`gg streaming…`, with the name's cell blank on the off frame), advancing once per text delta.
- **Borders and surfaces:** rounded borders (`BorderType::Rounded`) for boxes; full-frame `bg` fill; edge-to-edge padded bands on `surface_alt` for code and expanded output; edge-to-edge `selection` band for highlighted popup rows.
- **State signalling:**
  - Composer border: `border` when idle, `accent_soft` when busy, `accent` when stopping.
  - Footer crest: `gg` in `accent` at rest; the flashing name in `accent_soft` while a run works, `accent` on the crest frame only (the one frame of a turn that reads as the brand mark rather than as a working state); the trailing cell stays blank.
  - Footer `ctx N%`: `muted` at `<= 60%`, `warn` from `61%` to `85%`, `error` above `85%`.
  - Scrollbar: one column, each cell `▐` with `outline` behind it; the thumb in `accent`, the track in `border`.
- **Empty state (`src/tui/view/welcome.rs`):** three centred rows (name in `accent` bold, tagline in `muted`, key hints in `dim`), omitted on tiny viewports, cleared as soon as content appears.

## Visual motifs and emblems

When an SVG icon or emblem is needed (HTML headers, docs favicon, social cards), use a minimal geometric silhouette or tufted scarlet crest motif built strictly from palette colours (`#D94B4B`, `#E07A4F`, `#16191C`, `#E9E3D7`). Avoid cartoon birds, gradients, neon glows, or heavy drop shadows.

## Canonical source

`src/tui/theme.rs` is the single source of truth for the 16-role palette. `docs/theme/ganggang.css`, `docs/theme/favicon.svg`, and `src/export.rs` all implement this same palette.

## Verification checklist

1. All colours map to the 16 canonical roles in `src/tui/theme.rs`.
2. Luminance hierarchy is preserved (`bg < surface < surface_alt` and `dim < muted < text`).
3. HTML outputs are self-contained with zero external assets.
4. All copy and labels follow `.agents/skills/style-guide/SKILL.md`.
