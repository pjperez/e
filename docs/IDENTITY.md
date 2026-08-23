# e — Personality & Visual Identity (the neon pass)

*Goal: give `e` a memorable, stylish identity — cyberpunk / outrun / neon,
reminiscent of the "beautiful hacker" aesthetic. Not the ugly green-on-black
cliché, but the **stylish** neon version: *Drive*, *Kavinsky*, *Hotline
Miami*, *Ghost in the Shell*.*

---

## Design thesis

The current palette is already purple-on-near-black — half of outrun. The pass
completes it: **hot magenta + cyan + violet** on deep near-black, with **CRT /
terminal** accents (scanlines, glow, glitch, boot sequence). It should feel
alive, atmospheric, and unmistakably *hacker-cool* — while keeping text
high-contrast and legible.

**The rule:** neon is *atmosphere*, not noise. Glow lives on accents, borders,
and the logo — never on body text. Scanlines are barely-there. No animation
hurts reading. This keeps the "fast, slim, legible" promise intact.

---

## Palette (outrun core)

| token | value | use |
|-------|-------|-----|
| `--bg` | `#05060a` | deepest background |
| `--bg-2` | `#0a0c14` | panels / cards |
| `--bg-3` | `#121426` | raised surfaces |
| `--edge` | `#2a1f4d` | purple-tinted borders |
| `--accent` | `#ff2d95` | **outrun pink** — primary |
| `--accent-2` | `#22d3ee` | **cyan** — secondary / terminal |
| `--accent-3` | `#a855f7` | violet — tertiary |
| `--ok` | `#39ff14` | **matrix green** — success |
| `--warn` | `#ffb300` | neon amber |
| `--err` | `#ff2a4d` | neon red |
| `--text` | `#e8e9f5` | high-contrast body |
| `--text-dim` | `#9aa0c4` | secondary text |
| `--text-faint` | `#5f6480` | tertiary / hints |

---

## Signature effects (tasteful, not gaudy)

### 1. Neon glow
- **Logo mark** — pink glow + subtle glitch on load.
- **Status dot** — becomes a neon "LED" with a soft glow ring.
- **Focus / hover** — composer focus, model pill hover, suggestion hover, tool
  card hover all pick up a pink or cyan glow.
- **Empty-state mark** — a big glowing "e" with a halo.

### 2. CRT / terminal atmosphere
- **Scanline overlay** — a fixed, barely-visible repeating scanline texture over
  the whole app (`opacity` ~0.03–0.05, `pointer-events: none`). Never blocks
  interaction, never hurts readability.
- **Subtle vignette** — darkening at the edges for depth.
- **Blinking caret** — already exists; recolor to neon cyan with a glow.

### 3. Outrun grid (empty state)
- A synthwave **perspective grid horizon** behind the empty-state mark — the
  classic outrun sun-grid, done with a CSS gradient so it stays cheap.

### 4. Boot sequence (empty state)
- A brief typed **`e:// INITIALIZING…`** line on first open — a hacker boot
  moment. One line, ~1s, then fades to the normal empty state.

### 5. Glitch (logo only)
- A quick RGB-split glitch on the "e" mark when the app loads. One short burst,
  then it settles. Keeps the "alive" feel without being distracting.

---

## Typography

- Keep the system font stack for body (readability first).
- **Lean harder into monospace** for the hacker bits: role labels, tool names,
  status, model pill, hints, labels — already mono; make it feel intentional
  with letter-spacing and the neon colors.

---

## State colors (semantic, now neon)

| state | color | glow |
|-------|-------|------|
| idle | faint | none |
| busy / thinking | pink | pink pulse |
| tool pending | amber | amber |
| tool ok | matrix green | green |
| tool error | neon red | red |
| stop | neon red | red |

---

## What this does NOT do

- No green-on-black terminal cliché (that's the *ugly* hacker trope).
- No heavy animation that hurts reading or performance.
- No added chrome or layout change — same slim structure, new skin.
- No change to the "fast, slim, legible" promise — neon is atmosphere.

---

## Implementation notes

- All changes live in `src/style.css` (variables + a handful of effect rules)
  and a tiny bit of `index.html` / `main.ts` for the boot line and glitch.
- Zero new dependencies. Pure CSS + a couple of keyframes.
- Respect `prefers-reduced-motion` — disable glitch/pulse/scanline for users
  who ask for reduced motion.
