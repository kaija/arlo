# Arlo AI Design System

The shared design language of the **Arlo AI** open-source project family — arlo-ai.app and its three project sites. One accent (`#5856D6`), Inter, soft indigo-tinted neutrals, pill buttons, frosted-glass nav.

## Sources
Built from **https://github.com/kaija/arlo-web** (branch `main`) — the landing-site repo whose `styles.css` is the byte-identical design-system contract shared across all four Arlo sites. Explore it (and the sibling repos it links: `kaija/arlo`, `kaija/arlo-lite-ios`, `kaija/ag-ui-rust`, each with its own `website/` folder) to design against the real product.

## Product context
Arlo AI is a family of free, MIT-licensed open-source projects for running AI agents "on your own terms" — no vendor backend, ever:
- **Arlo Rust** — private, local-first agent runtime (TUI / CLI / embeddable library; macOS, Windows, Linux).
- **Arlo Lite** — standalone React Native iOS app that talks directly to your own LLM provider (BYO key). *Not* an Arlo Rust client.
- **AG-UI Rust** — Rust implementation of the AG-UI protocol; the standard interface between frontends and Arlo Rust.
Two independent paths: `frontends ↔ AG-UI Rust ↔ Arlo Rust ↔ provider`, and separately `Arlo Lite → provider`.

## Content fundamentals
- **Tone**: plain, confident, principle-driven. Short declaratives with a punch: "No backend of ours in between. Ever." / "Your keys, your machine." / "no strings."
- **Voice**: "we" for commitments ("What we won't compromise on", "We never see them"), "you/your" for the user's ownership. Explanations often justify themselves: "…because there is nowhere for them to go."
- **Casing**: sentence case everywhere — headings, buttons, links. No title case, no all-caps.
- **Punctuation**: em-dashes for asides, middle dots (·) as separators ("TUI · CLI", "Free & Open Source · MIT"), arrows in links ("Visit site →").
- **No emoji.** No exclamation marks. No marketing superlatives — value props are stated as facts.
- **i18n**: English lives in markup; zh-Hant and ja are overrides. Proper nouns/acronyms (Rust, iOS, MCP, SSE, AG-UI) stay untranslated; prose pills like "BYO key" get translated.

## Visual foundations
- **Color**: white page; alternating sections tinted `#f8f8fc`; a single indigo accent `#5856D6` (hover `#4745B5`); text ramp `#1a1a2e / #64648c / #8e8eaa` (all indigo-tinted, never pure gray/black). Accent tints via alpha: 8% fills, 15% glow rings. Borders are black-alpha hairlines (.06 / .1). No gradients, no imagery, no illustration — the site is entirely type + cards + inline SVG marks.
- **Type**: Inter 400–700 only (Google Fonts). Hero 3.5rem/700/−0.03em/1.1; section h2 2rem/700/−0.02em; card h3 1–1.125rem/600/−0.01em; body 16px/1.6; card body 14px; meta 13px. Mono (`SF Mono`/`Fira Code`/`JetBrains Mono` system stack — no webfont) appears **only** in pills.
- **Spacing**: 1120px container with 24px gutters; 100px section padding (hero 160px top for the fixed nav); 24px card grid gaps; 32px/28px card padding; 64px below section headers.
- **Radii**: checkbox 5px (`--radius-xs`); brand-mark rounded square 7px (`--radius-mark`); inputs, selects and 44px icon boxes 8px; alerts, toasts and stack nodes 12px; cards, tables and dialogs 16px; large panels 24px; buttons, badges, tags, tabs, pagination and avatars fully round (999px). No sharp corners anywhere. Icons themselves are unframed strokes with round caps/joins — the roundness comes from the container, not the glyph.
- **Shadows**: very soft black-alpha (sm→xl); highlighted elements get `shadow-md + 0 0 0 1px rgba(88,86,214,.15)` glow ring. No inner shadows.
- **Cards**: white, 1px hairline border, 16px radius, generous padding; **hover** = border→glow, md shadow + ring, translateY(−2px), 250ms ease. Link hover = opacity .7 or secondary→primary color, 150ms ease. No press/scale states, no bounces.
- **Transparency & blur**: only the fixed nav — `rgba(255,255,255,.82)` + `saturate(180%) blur(20px)`, hairline bottom border.
- **Layout**: fixed top nav (64px); centered copy-only hero on the hub (project sites use two-column hero + device/terminal mockup); 3-up card grids collapsing 2→1; centered stack diagram (max 640px) with unicode ↕/→ arrows.

## Iconography
- **Line icons**: 24px, 2px stroke, round caps/joins, `currentColor` (renders accent inside 44px tinted `radius-sm` boxes) — Feather-style, inlined as SVG. No icon font, no PNGs.
- **Brand marks**: each project = a white glyph in the shared `#5856D6` rx-7 rounded square (hub "A", rust ">", lite peak-with-bar, agui waveform) — in `assets/`, also as the `Logo` component. GitHub octocat inlined (16–20px, fill currentColor).
- **Unicode as UI**: ↕ → arrows in the stack diagram, · separators, → in links. No emoji ever.
- **AI action set** (`components/icons/` + `assets/icons/*.svg`, 28 glyphs): the marketing site itself ships no product-UI icons, so this set was authored to the same Feather-style geometry the site uses for its feature tiles — 24×24 box, 2px stroke, round caps/joins, no fills except the two solid glyphs (`send`, `zap`, `volume` wedge). Substitution flagged: geometry follows Feather (MIT), not an Arlo-original drawing.
  - **Composer**: `attach` `image` `camera` `voice` `file` `code` — idle at `--text-muted`, active/focused at `--color-accent`.
  - **Send/stop**: `submit` (arrow-up, primary filled button) while idle; swap to `stop` (square) while streaming — same button, no layout shift.
  - **Message actions**: `copy` `regenerate` `edit` `thumbs-up` `thumbs-down` `share` `download` at 16px, `--text-faint`, → `--text-body` on hover.
  - **Speech / audio**: `voice` (mic, input) vs `waveform` (activity, model is speaking) vs `volume` (playback control) — never interchange them.
  - **Chrome**: `chat` `new-chat` `search` `settings` `globe` `trash` `check` `close` `more`.
  - **Sizes**: 24 default · 20 in `sm` buttons · 16 inline with text · 32 with `strokeWidth={1.75}` · 44px tinted `radius-sm` box for feature tiles. Never scale a 24px glyph past 32px at 2px stroke.

## Components (window.ArloAIDesignSystem_6316a6)
- `components/brand/` — **Logo**, **GitHubIcon**
- `components/core/` — **Button**, **Pill**, **HeroBadge**, **SectionHeader**
- `components/cards/` — **ProjectCard**, **FeatureCard**, **StackNode**
- `components/navigation/` — **Nav**, **LangSwitch**, **Footer**
- `components/icons/` — **Icon** (+ `IconPaths`, `IconNames`)
- `components/forms/` — **Input**, **Select**, **Checkbox**, **Radio**, **Switch**, **Slider**
- `components/display/` — **Card**, **Avatar** (+ **AvatarGroup**), **Badge**, **Tag**, **Table**, **Progress**, **Spinner**
- `components/controls/` — **Tabs**, **Pagination**
- `components/feedback/` — **Alert**, **Toast** (+ **ToastStack**), **Tooltip**, **Dialog**
Each has a `.d.ts` props contract and a `.prompt.md` usage note. Inventory matches the source site exactly; **intentional additions** (requested by the user; not present in `kaija/arlo-web`, which is a marketing site):
- `Icon` — AI-app action glyphs, drawn to the site's existing line-icon geometry.
- `forms/`, `display/`, `controls/`, `feedback/` — an application-UI layer (19 components) for building product surfaces. Every value is derived from the marketing contract: same radii scale, same 150/250ms ease, same hairline borders, same accent-tint fills. Classes live in `css/ui.css`, kept separate from the byte-faithful `css/components.css`.
- Status colors (`--color-success` / `--color-warning` / `--color-danger` + 8% tints) and `--radius-xs` (5px) / `--radius-mark` (7px) — the source defines no semantic status palette; these are low-chroma and indigo-adjacent so they sit beside `#5856D6` without competing.

## Index
- `styles.css` — global entry (imports everything below)
- `tokens/` — `colors.css`, `typography.css`, `effects.css`, `fonts.css`
- `assets/icons/` — the 28 action glyphs as standalone SVG files (copy these into static HTML)
- `css/` — `base.css` (reset), `components.css` (the shared class contract, byte-faithful to the repo)
- `components/` — React primitives (above)
- `ui_kits/website/` — arlo-ai.app landing recreation (`index.html`, working EN/繁中/日本語 switch) + 404
- `guidelines/` — foundation specimen cards
- `assets/` — favicon + the four project marks
- `DESIGN.md` — the whole system in Google Labs' [DESIGN.md](https://github.com/google-labs-code/design.md) format (YAML tokens + prose rules) for AI coding agents; validate with `npx @google/design.md lint DESIGN.md`
- `github.md` — source-repo sync record · `SKILL.md` — agent skill entry point

## Notes & caveats
- Fonts load from Google Fonts (Inter); the repo ships no font binaries. Mono is a system-font stack by design.
- There is no standalone logo file upstream beyond `favicon.svg`; marks were copied verbatim from the site's inline SVGs, never redrawn.
