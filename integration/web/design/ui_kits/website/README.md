# Website UI kit — arlo-ai.app

Recreation of the Arlo AI hub landing site, composed from the design-system components (window.ArloAIDesignSystem_6316a6).

- **index.html** — the full landing page: fixed frosted nav, centered copy-only hero, three project cards, the two-path stack diagram, four principles, footer. The EN / 繁中 / 日本語 language switch works exactly like the real site (English is the fallback for any missing key).
- **Landing.jsx** — the page + full i18n dictionaries, lifted verbatim from index.html / i18n.js in kaija/arlo-web.
- **notfound.html** — the 404 page.

Fidelity notes: the hub hero is intentionally centered and copy-only (the project sites use a two-column hero with a device/terminal mockup). Analytics, meta/OG tags, and localStorage persistence of the language choice are omitted.
