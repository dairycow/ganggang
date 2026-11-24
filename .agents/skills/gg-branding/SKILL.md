---
name: gg-branding
description: "Apply the ganggang (gg) visual brand and gang-gang cockatoo theme to TUI views, HTML outputs, documentation sites, diagrams, and assets. Use whenever designing, styling, or updating any visual surface in this repository."
---

# ganggang Brand Guide

## Identity and naming

- **Project and crate name:** `ganggang` (always lowercase; never `GangGang` or `Gang-Gang`).
- **Binary and short mark:** `gg` (lowercase; bold in scarlet crest `#D94B4B` when used as a brand mark).
- **Tagline:** `a small, sharp agentic harness` (from `src/tui/view/welcome.rs` and `Cargo.toml`).
- **Subject:** the Australian gang-gang cockatoo (*Callocephalon fimbriatum*): charcoal-grey plumage, scarlet crest, orange-red cheeks, warm cream beak, pale grey wings with a grey-green covert wash, paired with Australian wattle gold for warnings.
- **Visual character:** restrained, utilitarian, dark-first. Charcoal grounds and warm off-white text carry most of the screen. Scarlet is reserved for `gg` marks, active selections, and stop/abort states. Orange-red marks live activity.

## Canonical 15-role colour palette

Source of truth: `src/tui/theme.rs` (`TRUECOLOR`) and `src/tui/theme/fallback.rs` (`indexed_palette`).

| Role | Hex | RGB | xterm-256 | Bird reference | Semantic usage |
|---|---|---|---|---|---|
| `bg` | `#16191C` | `22, 25, 28` | `234` | Charcoal plumage | Full-screen canvas background |
| `surface` | `#202429` | `32, 36, 41` | `235` | Raised plumage | Composer, footer, panels, cards, sidebars |
| `surface_alt` | `#2A2F35` | `42, 47, 53` | `236` | Raised slate | Code bands, expanded tool output, floating popups |
| `border` | `#3A4047` | `58, 64, 71` | `238` | Feather edge | Hairline rules, dividers, scrollbar track, idle box border |
| `selection` | `#3A2B2B` | `58, 43, 43` | `237` | Warm charcoal | Selected popup row and active list highlight |
| `text` | `#E9E3D7` | `233, 227, 215` | `254` | Warm cream beak | Primary body text and active model name |
| `muted` | `#98A0A6` | `152, 160, 166` | `247` | Pale grey wings | Secondary text, tool args, paths, token totals, idle status |
| `dim` | `#6B7379` | `107, 115, 121` | `243` | Shadow grey | Hints, placeholders, info lines, thinking, expanded output |
| `code` | `#C7D0C0` | `199, 208, 192` | `251` | Sage-tinted grey | Code block text on `surface_alt` |
| `accent` | `#D94B4B` | `217, 75, 75` | `167` | Scarlet crest | `gg` mark, `you │ ` prefix, popup `▸`, scrollbar thumb, stopping border, abort line, primary actions |
| `accent_soft` | `#E07A4F` | `224, 122, 79` | `173` | Orange-red cheek | Live states: tool calls, active spinner and `working`, busy composer border |
| `accent_deep` | `#8E3030` | `142, 48, 48` | `95` | Deep crest red | High-emphasis borders, active or pressed red accents |
| `ok` | `#A7B38F` | `167, 179, 143` | `144` | Grey-green wing wash | Success (`✓`), approved states, positive deltas |
| `warn` | `#D9A441` | `217, 164, 65` | `179` | Wattle gold | Warnings (`⚠`), watch-mode badge, context usage 61% to 85% |
| `error` | `#E06A6A` | `224, 106, 106` | `168` | Soft scarlet | Failed tool calls (`✗`), command errors, context usage above 85% |

### Palette invariants

- **Semantic roles only:** widgets, HTML templates, and stylesheets reference role names (`bg`, `surface`, `accent`, `muted`), not raw hex values scattered in logic.
- **Luminance order:** grounds step up (`bg < surface < surface_alt`); foregrounds step up (`dim < muted < text`).
- **Distinct 256-colour fallback:** all 15 roles map to distinct xterm-256 indices, claimed in priority order (marks and status first, then text and greys, then grounds).

## CSS custom properties and web typography

Use this `:root` block for standalone HTML outputs, reports, and web surfaces:

```css
:root {
  color-scheme: dark;
  --bg: #16191C;
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

- **Separators:** ` │ ` (U+2502) for footer fields and `you │ ` user prefix; ` · ` (U+00B7) for inline hints and metadata.
- **Markers:** `▸` and `▾` (collapsed and expanded tool calls; `▸ ` in `accent` on `selection` for active popup rows), `$ ` (shell command), `✓` (`ok`), `✗` (`error`), `⚠ ` (`warn`), `~ ` (`dim` italic for thinking), `■ run aborted` (`accent` bold), `○ ready` (`muted`), `↑in ↓out` (token totals), `• ` (list bullets), `…` (ellipsis), `[truncated: N lines]`.
- **Spinner:** 10-frame Braille cycle (`⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏`) at 100 ms ticks (`accent_soft` when working, `accent` when stopping).
- **Borders and surfaces:** rounded borders (`BorderType::Rounded`) for boxes; full-frame `bg` fill; edge-to-edge padded bands on `surface_alt` for code and expanded output; edge-to-edge `selection` band for highlighted popup rows.
- **State signalling:**
  - Composer border: `border` when idle, `accent_soft` when busy, `accent` when stopping.
  - Footer `ctx N%`: `muted` at `<= 60%`, `warn` from `61%` to `85%`, `error` above `85%`.
- **Empty state (`src/tui/view/welcome.rs`):** three centred rows (name in `accent` bold, tagline in `muted`, key hints in `dim`), omitted on tiny viewports, cleared as soon as content appears.

## Visual motifs and emblems

When an SVG icon or emblem is needed (HTML headers, docs favicon, social cards), use a minimal geometric silhouette or tufted scarlet crest motif built strictly from palette colours (`#D94B4B`, `#E07A4F`, `#16191C`, `#E9E3D7`). Avoid cartoon birds, gradients, neon glows, or heavy drop shadows.

## Canonical source vs legacy drift

`src/tui/theme.rs` is the single source of truth. Older files in the repository predate this palette and must not be copied for new visual work:

- `design.html`: Tokyo-Night blue (`#7aa2f7`)
- `src/export.rs`: blue (`#4a89dc`) on `#14161a`
- `session-analysis-01a207a2.html`: crimson (`#e84c3d`)
- `docs/book.toml`: default mdBook `coal` theme

## Verification checklist

1. All colours map to the 15 canonical roles in `src/tui/theme.rs`.
2. Luminance hierarchy is preserved (`bg < surface < surface_alt` and `dim < muted < text`).
3. HTML outputs are self-contained with zero external assets.
4. All copy and labels follow `.agents/skills/style-guide/SKILL.md`.
