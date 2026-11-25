# Milestone 6 colour spec

This is the colour half of [`m6.md`](m6.md) (branch `m6-theme`). It is written to be implemented as given: every
value is final, and every existing `Theme` field has a named replacement. Do not pick new colours while
implementing; if a value looks wrong on screen, record it under [Open questions](#open-questions) and keep going.

## What omp does

omp 19.1.1 ships 100 built-in themes (53 dark, 47 light); the default dark one is `titanium`. They were read from
the installed binary. Six things in them are worth taking; none of them is a particular hue.

1. **One hue family for every neutral.** Titanium's backgrounds, borders and greys are all the same cool blue-grey
   (`#0f1216`, `#151820`, `#1c2029`, `#2a3038`, `#6b7280`, `#9ca3b0`, `#e8ecf4`). Nothing is pure grey. xctx today
   mixes two families: its chrome is neutral (`#0d0e10`, `#15161a`) and its terminal is Tailwind zinc (`#18181b`),
   so a terminal pane reads as a grey box stuck onto the window (see `reviews/m6-baseline/home-empty.png`).
2. **Three named surface levels around the content.** Each theme names `chrome` (around the content), `background`
   (the content) and `widget` (above the content). Across the 53 dark themes the content sits at a median lightness
   of 7.3%; chrome is darker than it in 35 and equal in 17; widgets are lighter in 44, by a median of 3.7 points.
   Content is the middle layer and the eye lands on it.
3. **A second accent for numbers.** Titanium uses blue (`#00b4ff`) for anything interactive and gold (`#d4c090`) for
   read-outs: cost, spend, labels. Counts and money never compete with buttons.
4. **State as an opaque background tint, not only as text.** Pending, success and error blocks have their own
   backgrounds a few points off the surface (`#0f1216`, `#0f1216`, `#1a0f10`), so a failed block is visible from
   across the room without a red wall of text.
5. **An intensity ramp for "how much".** The thinking level runs grey, lighter grey, accent, gold
   (`#4a5058` → `#6a7078` → `#9ca3b0` → `#00b4ff` → `#d4c090`). The same ramp fits xctx's autonomy level.
6. **Agent colours are derived, not chosen per agent.** Six slots, assigned in arrival order and kept for the
   session; red is never an agent colour (it means error), and every slot must be at least 3:1 against every
   surface a name is drawn on. The terminal's 16 ANSI colours are part of the same theme, so program output matches
   the chrome.

Titanium's own status colours (`#00ff88`, `#ff4757`) are tuned for thin terminal glyphs. Used as fills and badges
in a window they glare, so the palette below keeps the structure and lowers their saturation.

## Tokens

`Theme` in `crates/xctx-ui/src/theme.rs` gets these fields. `Theme::dark()` and a new `Theme::light()` return the
two columns.

### Surfaces and borders

| Field | Dark | Light | Use |
| --- | --- | --- | --- |
| `chrome` | `#0f1216` | `#eceef2` | Window background, Roster, title/tab area, status line, split gaps |
| `surface` | `#151820` | `#f8f9fb` | Panes, the terminal background, cards, Inspector body |
| `raised` | `#1c2029` | `#ffffff` | Pane headers, inputs, resting buttons, hovered rows |
| `overlay` | `#232834` | `#ffffff` | Dialogs, the palette, the Inbox overlay, toasts, menus |
| `border_subtle` | `#1f252d` | `#e4e7ec` | Dividers inside a surface, split dividers at rest |
| `border` | `#2a3038` | `#d3d8e0` | Outlines of cards, inputs, panes |
| `border_strong` | `#3b424d` | `#b4bac6` | Outlines of overlays, hovered inputs |
| `scrim` | `#000000` at 50% | `#1a1e26` at 30% | Behind an overlay |

### Text

| Field | Dark | Light | Use | Contrast on `surface` |
| --- | --- | --- | --- | --- |
| `text` | `#e8ecf4` | `#1a1e26` | Primary text | 15.0 / 15.9 |
| `text_muted` | `#9ca3b0` | `#545b69` | Labels, inactive rows, secondary lines | 7.0 / 6.5 |
| `text_subtle` | `#6b7280` | `#737a89` | Placeholders, key hints, timestamps only | 3.7 / 4.1 |

`text_subtle` is below 4.5:1 on purpose. Never use it for anything the user must read to act.

### Accent, read-outs and status

| Field | Dark | Light | Use |
| --- | --- | --- | --- |
| `accent` | `#2ea8ff` | `#006db3` | Primary buttons, links, the focus ring, the caret, the selected item's marker |
| `accent_hover` | `#47b2ff` | `#005f9c` | A primary button under the mouse |
| `on_accent` | `#0f1216` | `#ffffff` | Text on an `accent`, `success` or `danger` fill |
| `selection` | `accent` at 16% | `accent` at 14% | Selected row background, text selection |
| `meta` | `#d4c090` | `#7a6118` | Read-outs: port, budget `3/8`, cost, counts, key chords in the palette |
| `success` | `#3ddc84` | `#157a46` | Running, passed, agreed |
| `warning` | `#ffb347` | `#9a5a00` | Needs you, paused, waiting for approval |
| `danger` | `#ff5c6a` | `#c6283a` | Failed, blocked, production, destructive actions |

Tinted elements (badges, gate rows, toasts) keep the existing rule in `Theme::tone`, with new alphas: text is the
tone colour, fill is the tone colour at 12%, outline is the tone colour at 40%. `Tone::Neutral` is `text_muted` on
`raised` with a `border` outline.

### Agent colours

| Field | Dark | Light |
| --- | --- | --- |
| `agent[0]` | `#b392f0` | `#6f42c1` |
| `agent[1]` | `#39d0c4` | `#0b7d76` |
| `agent[2]` | `#f47fb8` | `#b83280` |
| `agent[3]` | `#a8d672` | `#4d7c0f` |
| `agent[4]` | `#8aa4ff` | `#3b5bdb` |
| `agent[5]` | `#ff9e64` | `#b45309` |

The orchestrator is always `accent`. Every other agent of a space takes `agent[i % 6]`, where `i` is its position
in the space's graph in creation order, so an agent keeps its colour while the space lives. The colour marks the
agent's name and a 2px bar on the left of its pane header, Roster row, graph node and channel messages. It never
replaces a state colour: state is the glyph, identity is the bar.

### Terminal

`TerminalTheme` (`crates/xctx-term-view/src/theme.rs`) keeps its shape. `xctx-term-view` must not depend on
`xctx-ui`, so `Theme` carries the 16 ANSI colours and `crates/xctx` builds the `TerminalTheme` from the `Theme`.

| `TerminalTheme` field | From `Theme` |
| --- | --- |
| `background` | `surface` |
| `foreground` | `text` |
| `cursor` | `text` |
| `cursor_text` | `surface` |
| `selection` | `accent` at 33% |
| `scrollback_indicator` | `text_muted` at 40% |
| `ansi` | `ansi` (below) |

| ANSI | Dark | Light | | ANSI | Dark | Light |
| --- | --- | --- | --- | --- | --- | --- |
| 0 black | `#3b424d` | `#1a1e26` | | 8 bright black | `#6b7280` | `#737a89` |
| 1 red | `#ff5c6a` | `#c6283a` | | 9 bright red | `#ff8a94` | `#e0465a` |
| 2 green | `#3ddc84` | `#157a46` | | 10 bright green | `#7dedb0` | `#1f9d5c` |
| 3 yellow | `#f0c040` | `#8a6a00` | | 11 bright yellow | `#fddb72` | `#a67c00` |
| 4 blue | `#2ea8ff` | `#0073bd` | | 12 bright blue | `#6ec9ff` | `#2a8fd6` |
| 5 magenta | `#d06fed` | `#9b36b7` | | 13 bright magenta | `#e69dfc` | `#b65fd0` |
| 6 cyan | `#13d1cd` | `#0b7d86` | | 14 bright cyan | `#75efea` | `#1397a1` |
| 7 white | `#c3c9d3` | `#545b69` | | 15 bright white | `#f7f9fc` | `#9aa1ae` |

## Where the colours go

- **Layers.** `chrome` is the window; panes and cards are `surface` on top of it; headers and inputs are `raised`;
  anything floating is `overlay` with a `border_strong` outline over a `scrim`. A terminal pane and the card around
  it are the same `surface`, so the terminal no longer looks pasted in.
- **Focus.** The focused pane has a 1px `accent` outline and its header text is `text`; unfocused panes have a
  `border` outline and `text_muted` header text. The terminal's own colours never dim.
- **Production.** A space whose `MISE_ENV` includes `production` uses `danger` where other spaces use `accent` for
  the focused-pane outline, and its env segment in the status line is a `Tone::Danger` badge.
- **State glyphs** (Roster, pane headers, graph): running `success`, needs you `warning`, failed or blocked
  `danger`, idle `text_subtle`, paused `text_muted`.
- **Status line.** Labels are `text_subtle`. Values: branch `text`, env `text` (or the danger badge), port `meta`,
  agents `success` while any runs else `text_muted`, inbox count `warning` when above zero.
- **Loop stepper.** Done stages `text_muted`, the current stage `accent`, later stages `text_subtle`; a blocked
  stage is `danger`.
- **Autonomy** (the ramp from point 5): *ask each step* `text_muted`, *within budget* `accent`, *until checks
  pass* `meta`. The budget read-out beside it is always `meta`.
- **Gates.** A gate row's fill is its tone at 12%: plan and notice `Tone::Neutral`, widen and plugin write
  `Tone::Warning`, anything with production `Tone::Danger`. Only the default action of a row is a filled button;
  for a danger gate the filled button is **Review**, never **Approve**.
- **Buttons.** Primary: `accent` fill, `on_accent` text. Secondary: `raised` fill, `border` outline, `text`.
  Ghost: no fill, `text_muted`, `raised` under the mouse. Danger: `danger` text and 40% outline at rest, `danger`
  fill with `on_accent` text only inside a confirmation dialog.
- **Read-outs.** Every number the user did not type (port, counts, budget, cost, durations) is `meta`.

## Implementation steps

Do these in order, on branch `m6-theme` off `m6-base`. Run `mise run ci` after steps 2, 4 and 6.

1. **`crates/xctx-ui/src/theme.rs`: new fields.** Add the fields from the tables above, `agent: [Hsla; 6]` and
   `ansi: [Hsla; 16]`. Keep the `hex` helper. Add `Theme::light()`. Every public field and function gets a doc
   comment (see `AGENTS.md`).
2. **Rename the old fields everywhere they are read** (`crates/xctx-ui`, `crates/xctx-plugin-view`, `crates/xctx`).
   The only colour literal outside `theme.rs` in these crates is the dialog backdrop in
   `crates/xctx/src/dialogs.rs` (`hsla(0.0, 0.0, 0.0, 0.55)`), which becomes `scrim`. The rest is a rename:

   | Old field | New field |
   | --- | --- |
   | `bg` | `chrome` |
   | `surface` | `surface` |
   | `surface_hover` | `raised` |
   | `elevated` | `raised` for pane headers, active tabs and the prompt; `overlay` for dialogs, sheets and menus |
   | `border` | `border` |
   | `border_focused` | `accent` (and delete `Theme::outline`'s special field; it returns `accent` or `border`) |
   | `divider` | `border_subtle` |
   | `divider_hover` | `accent` |
   | `text`, `text_muted`, `text_subtle` | unchanged |
   | `accent` | `accent` |
   | `accent_muted` | `selection` |
   | `success`, `warning`, `danger` | unchanged |

   For `elevated`, decide per call site: if the element is drawn inside `render_dialog` or is a `*_dialog.rs`,
   `review_sheet.rs`, `fork_dialog.rs`, `plan_gate.rs` or `plugin_menu.rs` container, it is `overlay`; otherwise
   `raised`.
3. **`Theme::tone`.** Fill alpha 0.14 → 0.12, outline alpha 0.36 → 0.40, neutral fill `surface_hover` → `raised`.
   `ButtonVariant::colors` in `components.rs`: primary text becomes `on_accent`, primary hover becomes
   `accent_hover` (remove the `lighten` call for it).
4. **Terminal.** In `crates/xctx`, add `fn terminal_theme(theme: &Theme) -> TerminalTheme` using the mapping table,
   and use it where `workspace.rs` sets `terminal_theme: TerminalTheme::default()`. Leave
   `TerminalTheme::default()` in `xctx-term-view` as it is; its tests depend on it.
5. **Agent colours.** Add `Theme::agent_color(index: usize) -> Hsla` returning `agent[index % 6]`. Use it for the
   2px bar in pane headers (`split_view.rs`), graph nodes (`agent_graph.rs`) and channel messages
   (`shared_space.rs`); the index is the agent's position in `AgentGraph` creation order, and the orchestrator
   uses `accent`.
6. **System appearance.** At startup and when the window's appearance changes, set the global to `Theme::dark()`
   or `Theme::light()` and rebuild the terminal theme. Read how at the pinned GPUI revision (`v1.29.3`,
   `Window::appearance` and `observe_window_appearance`); do not guess the API.
7. **Tests.** Update the five tests in `theme.rs` and the colour tests in `components.rs` to the new fields. Add
   one test that every text and status colour of both themes is at least 4.5:1 against `surface` (3:1 for
   `text_subtle`), and one that no `agent` colour equals `danger`, `warning`, `success` or `accent`.

Done when: `mise run ci` passes; a terminal pane and its surrounding card are the same colour in a screenshot of
Home; switching macOS between Light and Dark restyles the open window without a restart.

## Not in this branch

Icons, motion, fonts, layout, the Roster and every other shell change in `m6.md`. Theme files on disk and a theme
picker. More than two themes.

## Open questions

- `agent[5]` (`#ff9e64`) is close in hue to `warning` (`#ffb347`). They never share a role (bar against glyph),
  but if they read alike on screen, replace `agent[5]` with ANSI bright cyan `#75efea`.
- Light `text_subtle` on `chrome` is 3.7:1. If status-line labels are hard to read in light mode, use `text_muted`
  there.
