<!-- SEED: re-run $impeccable document once there's code to capture the actual tokens and components. -->
---
name: Denia
description: Operator dashboard for a Docker-free, single-node PaaS. Mono-forward, dark-primary, dual-state palette.
---

# Design System: Denia

## 1. Overview

**Creative North Star: "Stagecraft and Breakdown"**

Named for the dual-form character the project borrows its palette from: a Stagecraft (pink) face and a Breakdown (purple) face over a black-hole-dark base. The dashboard reads the same way. Most of the surface is quiet, near-monochrome dark, tinted faintly toward violet. Color appears only when the machine has something to say: a service comes up, a route flips, a deploy breaks. The pink is the performance, the calm steady state; the purple is the fault, the state that demands attention. Color is signal, never decoration.

This is an operator tool, not a stage. It is mono-forward and dense. It shows real runtime state (cgroup/procfs metrics, route health, deploy progress) without abstraction. A solo homelab operator opens it to do one thing and leave, never to fight it.

It explicitly rejects: AWS/GCP console bloat (nested tabs, walls of config), Heroku/Vercel marketing-as-app, generic admin-template card grids, glossy crypto/neon dark, and neo-brutalism. The pink and purple are muted and rare, never glowing; this is the line that keeps a two-accent dark theme from becoming crypto slop.

**Key Characteristics:**
- Dark-primary, near-monochrome surface tinted toward the brand violet.
- Two semantic accents (pink = steady/Stagecraft, purple = fault/Breakdown) used sparingly.
- Mono-forward: monospace carries labels, IDs, metrics, tables; sans for prose only.
- Restrained motion: state transitions only, no entrances or choreography.
- Density over decoration. Every element helps the operator act or understand.

## 2. Colors

Strategy: restrained dark base + two reserved accents. Neutrals are tinted toward the violet brand hue (chroma ~0.01), never pure `#000`/`#fff`. Light theme ships as a toggle; dark is primary. Values below are proposed OKLCH anchors `[to be resolved during implementation]`.

### Primary
- **Stagecraft Pink** (`oklch(72% 0.15 350)` proposed): the steady-state / healthy / primary-action accent. Deliberately desaturated rose, not hot magenta. Used on <=10% of any screen.

### Secondary
- **Breakdown Violet** (`oklch(56% 0.14 305)` proposed): the fault / attention / breakdown-state accent. Pairs with error semantics, not decorative emphasis.

### Tertiary
- Conventional state hues for metrics and logs, tuned to sit beside the accents without clashing: warn `oklch(78% 0.13 75)` (amber), error escalates to Breakdown Violet, ok can borrow Stagecraft Pink or a muted green `oklch(70% 0.12 150)` `[choose one at implementation]`.

### Neutral
- **Black-hole Base** (`oklch(16% 0.012 315)` proposed): primary dark background, faint violet tint.
- **Surface** (`oklch(21% 0.012 315)` proposed): raised panels, table rows, input wells.
- **Border / Divider** (`oklch(30% 0.012 315)` proposed): 1px hairlines only.
- **Text** (`oklch(92% 0.008 315)` proposed) / **Muted Text** (`oklch(68% 0.01 315)` proposed).

### Named Rules
**The Signal Rule.** Pink and Violet mean something: steady vs. fault. They never appear as decoration, gradients, or brand flourish. If a color isn't reporting machine state or marking the one primary action, it's neutral.

**The No-Glow Rule.** Accents are muted OKLCH at moderate lightness. No neon, no glow, no `box-shadow` color halos. A glowing pink-on-black UI is the crypto anti-reference; forbidden.

## 3. Typography

**Display/UI Font:** `[mono family to be chosen at implementation]` (candidates: Berkeley Mono, JetBrains Mono, Commit Mono). Fallback: `ui-monospace, SFMono-Regular, monospace`.
**Body/Prose Font:** `[technical sans to be chosen]` for docs and longer prose only. Fallback: `system-ui, sans-serif`.

**Character:** Mono-forward. Monospace is the default voice of the UI; it aligns numbers in metric tables, gives IDs and routes a terminal-native read, and reinforces the operator register. Sans appears only where prose runs long.

### Hierarchy
- **Display** (`[weight TBD]`, clamp ~1.75–2.25rem): page/section titles, sparse.
- **Title** (`[weight TBD]`, ~1.125rem): panel and table-group headers.
- **Body** (`[weight TBD]`, ~0.875–0.9375rem, line-height ~1.5): values, labels, table cells. Prose capped 65–75ch.
- **Label** (`[weight TBD]`, ~0.75rem, letter-spacing ~0.04em, uppercase): metadata keys, status chips, column heads.

Maintain >=1.25 scale/weight contrast between steps. No flat scales.

### Named Rules
**The Aligned-Number Rule.** All metrics, IDs, ports, and durations render in mono with tabular figures so columns align without manual padding.

## 4. Elevation

Flat by default, matching restrained motion. Depth comes from tonal layering (Black-hole Base -> Surface), not shadow. The dark base already reads as depth; stacking shadows on top is the "2014 app" tell. Reserve any shadow for transient overlays (command palette, menus) only `[shadow vocabulary to be defined at implementation]`.

### Named Rules
**The Flat-By-Default Rule.** Surfaces are flat at rest. Layering is tonal, not cast. Shadow is allowed only on floating, dismissable overlays.

## 5. Components

No components exist yet (pre-implementation). This section will be populated, with a `.impeccable/design.json` sidecar, on the next scan-mode run once dashboard code exists.

## 6. Do's and Don'ts

### Do:
- **Do** keep the surface near-monochrome dark; let Stagecraft Pink and Breakdown Violet appear only as state signal or the single primary action.
- **Do** use monospace with tabular figures for all metrics, IDs, ports, and durations.
- **Do** tint every neutral faintly toward the violet hue (chroma ~0.01); never use `#000` or `#fff`.
- **Do** convey depth through tonal layering; keep surfaces flat at rest.
- **Do** honor `prefers-reduced-motion`; motion is state-feedback only and never required to understand the UI.

### Don't:
- **Don't** let pink/violet glow, gradient, or halo. The **No-Glow Rule** keeps this off the glossy crypto/neon dark anti-reference.
- **Don't** build AWS/GCP console bloat: nested tabs, walls of config, slow enterprise sprawl.
- **Don't** import Heroku/Vercel marketing-as-app gradients or oversized empty space into the tool.
- **Don't** ship a generic dashboard template: same-sized stat-tile card grid, boilerplate sidebar + topbar.
- **Don't** use neo-brutalism: thick black borders, hard drop shadows, clashing primaries.
- **Don't** use accent color for decoration. If it isn't reporting state or marking the primary action, it's neutral (**The Signal Rule**).
