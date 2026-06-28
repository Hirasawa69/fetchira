---
name: fetchira-design
description: Use this skill to generate well-branded interfaces and assets for fetchira, either for production or throwaway prototypes/mocks/etc. Contains essential design guidelines, colors, type, fonts, assets, and UI kit components for prototyping.
user-invocable: true
---

Read the `readme.md` file within this skill, and explore the other available files.

fetchira is a developer tool — a quota-aware router that fans web search / scrape /
research calls across many free-tier providers, used by AI coding agents. The dashboard
this system dresses is a **local instrument panel** (runs on `127.0.0.1`): dark slate,
hairline edges, one electric-lime "fuel" accent, monospace numbers, mechanical motion.

If creating visual artifacts (slides, mocks, throwaway prototypes, etc), copy assets out
of `assets/` and create static HTML files for the user to view. If working on production
code, copy assets and read the rules here to become an expert in designing with this brand.

Key files:
- `readme.md` — full design guide: personality, voice, visual foundations, anti-slop rules.
- `styles.css` + `tokens/` — link `styles.css` to inherit all CSS custom properties.
- `components/` — React primitives (Button, Badge, Card, StatusDot, QuotaMeter,
  ProviderCard, RouteLogLine, Input, Select, Tabs). Mount via
  `window.FetchiraDesignSystem_6526df` after loading `_ds_bundle.js`.
- `ui_kits/dashboard/` — the full router dashboard recreation (Overview / Accounts / Activity).
- `guidelines/` — foundation specimen cards.

Non-negotiables: Space Grotesk / Hanken Grotesk / JetBrains Mono (never Inter); no
gradients-as-fills; tight `5–7px` radii; electric-lime accent with green/amber/red/grey
gauge semantics; mono tabular numbers; no emoji; lowercase provider nouns, Sentence-case
labels. The quota meter is the hero — lead with it.

If the user invokes this skill without any other guidance, ask them what they want to
build or design, ask some questions, and act as an expert designer who outputs HTML
artifacts _or_ production code, depending on the need.
