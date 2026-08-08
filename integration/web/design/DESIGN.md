---
version: alpha
name: Arlo AI
description: Design system of the Arlo AI open-source project family — local-first AI agent runtimes. White page, one indigo accent, Inter, indigo-tinted neutrals, pill buttons, frosted-glass nav. No gradients, no imagery, no emoji.
colors:
  primary: "#5856D6"
  primary-hover: "#4745B5"
  primary-container: "rgba(88,86,214,0.08)"
  primary-glow: "rgba(88,86,214,0.15)"
  on-primary: "#ffffff"
  secondary: "#64648c"
  tertiary: "#8e8eaa"
  neutral: "#1a1a2e"
  surface: "#ffffff"
  surface-section: "#f8f8fc"
  surface-sunken: "#f0f0f8"
  on-surface: "#1a1a2e"
  on-surface-variant: "#64648c"
  outline: "rgba(0,0,0,0.06)"
  outline-strong: "rgba(0,0,0,0.1)"
  success: "#1f9d6b"
  success-container: "rgba(31,157,107,0.08)"
  warning: "#c07a12"
  warning-container: "rgba(192,122,18,0.08)"
  error: "#d1435b"
  error-container: "rgba(209,67,91,0.08)"
  scrim: "rgba(26,26,46,0.4)"
typography:
  headline-display:
    fontFamily: Inter
    fontSize: 3.5rem
    fontWeight: 700
    lineHeight: 1.1
    letterSpacing: -0.03em
  headline-lg:
    fontFamily: Inter
    fontSize: 2rem
    fontWeight: 700
    lineHeight: 1.2
    letterSpacing: -0.02em
  headline-md:
    fontFamily: Inter
    fontSize: 1.125rem
    fontWeight: 600
    lineHeight: 1.4
    letterSpacing: -0.01em
  body-lg:
    fontFamily: Inter
    fontSize: 1.125rem
    fontWeight: 400
    lineHeight: 1.7
    letterSpacing: "0em"
  body-md:
    fontFamily: Inter
    fontSize: 1rem
    fontWeight: 400
    lineHeight: 1.6
    letterSpacing: "0em"
  body-sm:
    fontFamily: Inter
    fontSize: 0.875rem
    fontWeight: 400
    lineHeight: 1.6
    letterSpacing: "0em"
  label-lg:
    fontFamily: Inter
    fontSize: 0.9375rem
    fontWeight: 500
    lineHeight: 1.4
    letterSpacing: "0em"
  label-md:
    fontFamily: Inter
    fontSize: 0.8125rem
    fontWeight: 600
    lineHeight: 1.3
    letterSpacing: -0.01em
  label-sm:
    fontFamily: Inter
    fontSize: 0.75rem
    fontWeight: 600
    lineHeight: 1.3
    letterSpacing: 0.01em
  code-pill:
    fontFamily: SF Mono
    fontSize: 0.8125rem
    fontWeight: 500
    lineHeight: 1.3
    letterSpacing: "0em"
rounded:
  none: 0px
  xs: 5px
  mark: 7px
  sm: 8px
  md: 12px
  lg: 16px
  xl: 24px
  full: 999px
spacing:
  xs: 4px
  sm: 8px
  md: 16px
  lg: 24px
  xl: 32px
  2xl: 64px
  section: 100px
  container: 1120px
components:
  button-primary:
    backgroundColor: "{colors.primary}"
    textColor: "{colors.on-primary}"
    typography: "{typography.label-lg}"
    rounded: "{rounded.full}"
    padding: 10px 20px
  button-primary-hover:
    backgroundColor: "{colors.primary-hover}"
    textColor: "{colors.on-primary}"
    shadow: 0 4px 16px rgba(88,86,214,0.3)
  button-secondary:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.on-surface}"
    typography: "{typography.label-lg}"
    rounded: "{rounded.full}"
    padding: 10px 20px
    border: 1px solid {colors.outline-strong}
  button-secondary-hover:
    backgroundColor: "{colors.surface-sunken}"
    textColor: "{colors.on-surface}"
  button-sm:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.on-surface}"
    typography: "{typography.label-md}"
    rounded: "{rounded.full}"
    padding: 6px 14px
  card:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.on-surface}"
    rounded: "{rounded.lg}"
    padding: 32px 28px
    border: 1px solid {colors.outline}
  card-hover:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.on-surface}"
    shadow: 0 4px 12px rgba(0,0,0,0.06), 0 0 0 1px {colors.primary-glow}
    transform: translateY(-2px)
  input:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.on-surface}"
    typography: "{typography.label-lg}"
    rounded: "{rounded.sm}"
    padding: 10px 14px
    border: 1px solid {colors.outline-strong}
  input-focus:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.on-surface}"
    borderColor: "{colors.primary}"
    shadow: 0 0 0 3px {colors.primary-container}
  input-error:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.on-surface}"
    borderColor: "{colors.error}"
  badge-accent:
    backgroundColor: "{colors.primary-container}"
    textColor: "{colors.primary}"
    typography: "{typography.label-sm}"
    rounded: "{rounded.full}"
    padding: 4px 10px
  badge-neutral:
    backgroundColor: "{colors.surface-sunken}"
    textColor: "{colors.secondary}"
    typography: "{typography.label-sm}"
    rounded: "{rounded.full}"
    padding: 4px 10px
  badge-success:
    backgroundColor: "{colors.success-container}"
    textColor: "{colors.success}"
    rounded: "{rounded.full}"
    padding: 4px 10px
  badge-warning:
    backgroundColor: "{colors.warning-container}"
    textColor: "{colors.warning}"
    rounded: "{rounded.full}"
    padding: 4px 10px
  badge-error:
    backgroundColor: "{colors.error-container}"
    textColor: "{colors.error}"
    rounded: "{rounded.full}"
    padding: 4px 10px
  tag:
    backgroundColor: "{colors.surface-sunken}"
    textColor: "{colors.secondary}"
    typography: "{typography.label-md}"
    rounded: "{rounded.full}"
    padding: 5px 12px
    border: 1px solid {colors.outline}
  pill-mono:
    backgroundColor: "{colors.primary-container}"
    textColor: "{colors.primary}"
    typography: "{typography.code-pill}"
    rounded: "{rounded.full}"
    padding: 5px 12px
  nav:
    backgroundColor: "rgba(255,255,255,0.82)"
    textColor: "{colors.on-surface-variant}"
    height: 64px
    backdropFilter: saturate(180%) blur(20px)
    border: 0 0 1px 0 solid {colors.outline}
  table-header:
    backgroundColor: "{colors.surface-section}"
    textColor: "{colors.tertiary}"
    typography: "{typography.label-sm}"
    padding: 12px 20px
  table-cell:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.on-surface}"
    typography: "{typography.body-sm}"
    padding: 14px 20px
  alert:
    backgroundColor: "{colors.primary-container}"
    textColor: "{colors.on-surface}"
    rounded: "{rounded.md}"
    padding: 14px 16px
  dialog:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.on-surface}"
    rounded: "{rounded.lg}"
    padding: 28px
    shadow: 0 16px 48px rgba(0,0,0,0.1)
  icon-tile:
    backgroundColor: "{colors.primary-container}"
    textColor: "{colors.primary}"
    rounded: "{rounded.sm}"
    size: 44px
  avatar:
    backgroundColor: "{colors.primary-container}"
    textColor: "{colors.primary}"
    typography: "{typography.label-md}"
    rounded: "{rounded.full}"
    size: 36px
---

## Overview

Arlo AI is a family of free, MIT-licensed open-source projects for running AI agents on your own terms — no vendor backend, ever. The design language is **engineering restraint with a single point of warmth**: a white page, indigo-tinted neutrals, and exactly one accent colour that carries every interactive signal.

The feel is a well-documented open-source project, not a SaaS funnel. Confident and plain. Nothing decorative earns its place — the entire surface is type, hairline-bordered cards, and thin inline SVG marks. Where other systems reach for a gradient or a hero illustration, Arlo reaches for whitespace.

Voice follows the same discipline: short declaratives ("Your keys, your machine."), "we" for commitments and "you/your" for the user's ownership, sentence case everywhere, em-dashes for asides, middle dots (·) as separators, arrows (→) in links. Never a superlative, never an exclamation mark, never an emoji.

## Colors

- **Primary (#5856D6):** The single indigo accent. The *only* driver of interaction — buttons, links, focus rings, active tabs, icon tiles, progress fills. Hover deepens to `#4745B5`. Never used as a large background fill except on a primary button.
- **Primary container (rgba(88,86,214,.08)):** 8% accent tint for icon tiles, badges, avatar fills, focus halos. The accent's only "surface" form.
- **Primary glow (rgba(88,86,214,.15)):** 15% accent, used exclusively as a 1px ring paired with a shadow on hovered/elevated cards.
- **Neutral / on-surface (#1a1a2e):** Indigo-tinted ink for headlines and body copy. Never pure black.
- **Secondary (#64648c):** Muted indigo-slate for subtitles, body prose in cards, nav links at rest.
- **Tertiary (#8e8eaa):** Faintest step — table headers, meta lines, placeholders, disabled text.
- **Surface (#ffffff):** The page and every card.
- **Surface section (#f8f8fc):** Barely-there tint that alternates full-bleed page sections and fills table headers.
- **Surface sunken (#f0f0f8):** Inert tracks and neutral chips — slider rails, progress troughs, switch off-state, secondary-button hover.
- **Outline (rgba(0,0,0,.06)) / outline-strong (rgba(0,0,0,.1)):** Black-alpha hairlines. Borders are alpha, never a grey hex, so they sit correctly on both white and tinted sections.
- **Success (#1f9d6b) / Warning (#c07a12) / Error (#d1435b):** Deliberately low-chroma so they read as *status*, not as competing brands. Each pairs with its 8% container tint. Use only for state — never as accents, never for emphasis.

The whole ramp is indigo-adjacent. Any new colour must be desaturated enough to sit beside `#5856D6` without competing with it.

## Typography

**Inter** is the only webfont, weights 400–700, loaded from Google Fonts. A mono system stack (`SF Mono` / `Fira Code` / `JetBrains Mono`) appears **only inside pills** — never for body copy, never for headings.

- **headline-display (3.5rem / 700 / −0.03em / 1.1):** Page hero only, one per page.
- **headline-lg (2rem / 700 / −0.02em):** Section headers.
- **headline-md (1.125rem / 600 / −0.01em):** Card and dialog titles.
- **body-lg (1.125rem / 400 / 1.7):** Hero and section subtitles.
- **body-md (1rem / 400 / 1.6):** Default prose.
- **body-sm (0.875rem / 400 / 1.6):** Card body, table cells.
- **label-lg (0.9375rem / 500):** Button and input text.
- **label-md (0.8125rem / 600 / −0.01em):** Field labels, tags, avatar initials.
- **label-sm (0.75rem / 600 / +0.01em):** Badges, table headers — the only place letter-spacing goes positive.

Negative tracking scales with size: the bigger the type, the tighter. Nothing is uppercase, and nothing is centred except the hub hero and the stack diagram.

## Layout

Fixed-max-width grid: **1120px container, 24px gutters**, centred. Sections get **100px vertical padding** (the hero gets 160px top to clear the 64px fixed nav), with 64px between a section header and its content.

Spacing runs on an 8px scale (4px half-step for micro-adjustments): 4 · 8 · 16 · 24 · 32 · 64. Card grids are 3-up with 24px gaps, collapsing 3→2→1. Card padding is 32px vertical / 28px horizontal; compact cards 20px.

The nav is fixed at 64px tall, full-width, frosted. Diagrams are centred and capped at 640px.

## Elevation & Depth

Depth comes from **tonal layers and hairlines first, shadow second**. The page is white, sections alternate to `#f8f8fc`, and cards sit on top as pure white with a 1px black-alpha border. That border does most of the hierarchy work.

Shadows are very soft, black-alpha, and used sparingly:

- `sm` — `0 1px 2px rgba(0,0,0,.04)` — knobs and thumbs
- `md` — `0 4px 12px rgba(0,0,0,.06)` — hovered cards, popovers
- `lg` — `0 8px 32px rgba(0,0,0,.08)` — toasts, dropdowns
- `xl` — `0 16px 48px rgba(0,0,0,.1)` — dialogs
- accent — `0 4px 16px rgba(88,86,214,.3)` — hovered primary button only

Elevated and hovered elements pair `md` with a `0 0 0 1px rgba(88,86,214,.15)` glow ring. There are no inner shadows. Transparency and blur appear in exactly one place — the fixed nav (`rgba(255,255,255,.82)` + `saturate(180%) blur(20px)`).

Motion: `150ms ease` for colour/opacity, `250ms ease` for transforms and multi-property transitions. Card hover lifts `translateY(-2px)`. No bounces, no scale-on-press, no spring easing.

## Shapes

Softness scaled to size, and **no sharp corners anywhere**:

- **5px (xs)** — 16–20px controls: checkbox boxes, small ticks
- **7px (mark)** — the rounded square inside every Arlo brand mark
- **8px (sm)** — inputs, selects, 44px icon tiles, square avatars
- **12px (md)** — alerts, toasts, stack-diagram nodes
- **16px (lg)** — cards, tables, dialogs
- **24px (xl)** — large panels
- **999px (full)** — buttons, badges, tags, tabs, pagination, avatars, all progress tracks

Icons are unframed 24×24 strokes at 2px with round caps and joins (Feather-style geometry) — the roundness comes from the container, not the glyph. Sizes: 24 default, 20 in small buttons, 16 inline with text, 32 at 1.75px stroke. Never scale a 24px glyph past 32px at full stroke weight.

## Do's and Don'ts

- **Do** use `#5856D6` as the only interactive colour — one primary action per screen.
- **Don't** introduce a second accent, a gradient, or any saturated colour that competes with the indigo.
- **Do** state value as fact in sentence case. **Don't** use title case, all-caps, superlatives, exclamation marks, or emoji — ever.
- **Do** reach for a hairline border and a tonal background shift before reaching for a shadow.
- **Don't** put a shadow on a resting card; shadow is a hover/overlay signal only.
- **Do** pair every shadow above `sm` with the accent glow ring when the element is interactive.
- **Do** keep borders as black-alpha (`rgba(0,0,0,.06)` / `.1`) so they work on white and on tinted sections. **Don't** substitute a grey hex.
- **Do** tint text with indigo (`#1a1a2e` / `#64648c` / `#8e8eaa`). **Don't** use pure black, pure grey, or `#000`.
- **Do** reserve success/warning/error strictly for state. **Don't** use error red as a decorative accent.
- **Do** use the mono stack inside pills only. **Don't** set body copy, labels, or headings in mono.
- **Don't** mix radii scales in one element group — a 16px card does not contain an 8px badge; badges are pill.
- **Do** use 100px section padding and the 1120px container; **don't** invent one-off section rhythms.
- **Do** distinguish the speech icons: `voice` (mic input) vs `waveform` (model speaking) vs `volume` (playback). **Don't** interchange them.
- **Do** swap `submit` (arrow-up) → `stop` (square) in place while streaming, same button, no layout shift.
- **Don't** add imagery, illustration, or stock photography — the system is type, cards, and line marks by design.
- **Do** maintain WCAG AA contrast (4.5:1 body text); `#8e8eaa` is for meta and placeholders only, never for prose.
- **Don't** use more than two font weights in a single component.
