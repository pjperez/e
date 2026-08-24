# Design

`e` is a reading surface first. A run produces a lot of output — streamed prose,
reasoning, tool cards, diffs — and the interface's job is to keep all of it
legible while telling you, at a glance, what the agent is doing.

Three rules follow from that:

- **One glance, one answer.** The agent's state should be readable in under a
  second, without parsing text.
- **Progressive disclosure.** The default view stays clean; detail is one click
  away. Tool cards collapse, reasoning folds, output truncates.
- **Atmosphere on the edges, never on the text.** Colour and glow live on
  accents, borders and the mark. Body text stays high-contrast and plain.

Everything is one stylesheet, `src/style.css`, with no framework and no build
step beyond the bundler.

## Themes

Two themes, switched by `html[data-theme]` and persisted across restarts. Dark
is the default.

| token | dark | light | use |
|-------|------|-------|-----|
| `--bg` | `#0a0b0e` | `#f6f6f8` | app background |
| `--bg-2` | `#0e1015` | `#ffffff` | panels, cards |
| `--bg-3` | `#14171f` | `#ececf1` | raised surfaces, inputs |
| `--edge` | `#1d212b` | `#e2e2ea` | borders |
| `--text` | `#e7e9ee` | `#16171c` | body |
| `--text-dim` | `#9aa1af` | `#565a66` | secondary |
| `--text-faint` | `#6b7280` | `#8a8f9c` | hints, metadata |
| `--accent` | `#a78bfa` | `#6d4fd1` | primary — violet |
| `--accent-2` | `#6ee7b7` | — | secondary — mint, success |
| `--warn` | `#fbbf24` | — | pending, caution |
| `--err` | `#f87171` | — | failure |
| `--code-bg` / `--code-fg` | `#12151c` / `#6ee7b7` | `#f0f0f5` / `#0f766e` | code |

## The glow layer

Over the base palette sits a thin accent layer: pink and cyan radial washes on
the background, and a soft pink glow (`--glow-pink`) on the mark, the status
LED, and hover states for icon buttons and the model pill.

It is deliberately narrow. The glow never touches body text, and in the light
theme it resolves to `none` — a bloom that reads as atmosphere on near-black
reads as a smudge on white.

## Layout

Two width tokens do most of the work, and the distinction between them matters:

```css
--content-width: clamp(48rem, 64vw, 72rem);
--measure: 50rem;
```

`--content-width` is the column everything aligns to — turns, activity strip,
composer, banners. It grows with the window rather than pinning to a fixed
width, which left wide displays mostly empty, but stays capped so a line never
runs long enough for the eye to lose its place on the return sweep.

`--measure` is narrower and applies to running prose only. The wider column
exists for code blocks, tool cards and tables, which earn the pixels; a
paragraph does not, so it stops earlier.

Corners are a single `--radius: 14px`.

## Typography

The body uses the platform's own UI stack — text should look native, not
branded. Monospace carries the machine-facing parts: role labels, tool names,
the model pill, status, token counts, and code.

```css
--font: -apple-system, BlinkMacSystemFont, "Segoe UI", Inter, Roboto, …
--mono: ui-monospace, "Cascadia Code", "JetBrains Mono", SFMono-Regular, …
```

That split is the main typographic signal: prose is for you, mono is what the
machine did.

## State

Status is colour plus motion, consistently:

| state | colour | motion |
|-------|--------|--------|
| idle | faint | none |
| working | `--accent` | pulsing dot |
| tool pending | `--warn` | pulsing rail on the card |
| tool succeeded | `--accent-2` | none |
| tool failed / stopped | `--err` | none |
| awaiting approval | `--warn` | spinner hidden — it is waiting on you |
| retrying a throttled provider | `--warn` | spinner runs, wait counts down |

The last two are worth calling out: a run parked on an approval prompt is not
working, so its spinner stops; a run backing off a rate limit *is* still alive,
so its spinner continues and the strip counts the wait down rather than
appearing to hang.

## The mark

The lowercase `e` is generated, not drawn. Its bowl is a true logarithmic
spiral, `r(θ) = r₀·e^(bθ)`, with the growth rate tuned so the radius increases
by exactly φ across the sweep. [`design/logo.py`](../design/logo.py) emits every
asset — mark, tile, banner, favicon — from the same equations, and
`public/e.svg` is the single source of truth in the app: the title bar and empty
state tint it with a CSS mask, so it follows the theme for free.

## Known gaps

- **No `prefers-reduced-motion` handling.** Several looping animations (the
  status pulse, the thinking pulse, the tool rail, the activity spinner) run
  regardless of the user's motion preference. They should be disabled when it is
  set.
