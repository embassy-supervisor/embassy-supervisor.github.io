# embassy-supervisor.github.io

The documentation website for
[embassy-supervisor](https://github.com/cedrivard/embassy-supervisor), built
with [Astro](https://astro.build) and
[Starlight](https://starlight.astro.build). Content lives as Markdown under
`src/content/docs/`; diagrams are Mermaid fences rendered client-side in a
light and dark "instrument panel" theme derived from the graph diagrams the
crate's own tooling emits.

## Run it locally

Requires [Bun](https://bun.sh) (or npm; adjust the commands):

```console
bun install
bun run dev        # dev server with hot reload
bun run build      # static build into dist/
bun run preview    # serve the built site
node tools/check-links.mjs dist   # audit internal links
```

## How it is put together

- `src/content/docs/` - all pages, grouped `getting-started/`, `concepts/`,
  `guides/`, `reference/`. The sidebar in `astro.config.mjs` references them
  by slug.
- `src/styles/theme.css` - the whole visual identity: color tokens for both
  variants, typography (IBM Plex Sans / Mono, self-hosted via Fontsource),
  the diagram figure chrome, cards, chips, and the landing page.
- `src/scripts/mermaid.ts` - the diagram renderer. It finds
  ```` ```mermaid ```` fences, re-renders them on theme flips, and adds the
  draw-in / dash-march motion (skipped under `prefers-reduced-motion`).
- `src/components/Head.astro` - loads the renderer lazily, only on pages
  that carry a diagram.
- `src/pages/index.astro` - the landing page, outside the docs layout.

### Diagram conventions

Diagrams declare semantic node classes and the renderer maps them to colors
per theme:

| class | meaning |
|---|---|
| `task` | a supervised node (`Terminate`) |
| `pool` | a pool (subroutine shape `[[".."]]`) |
| `provider` | a node that builds values others consume |
| `paused` | a `Pause` node (dashed) |
| `disabled` | control-started / dormant (faded, fine dashes) |
| `signal` | a shared static data flows through |
| `resource` | a resource slot (`@{ shape: notch-rect }`) |

Write fences with those classes (`NET["NET · task"]:::task`); never hardcode
colors in the source, so both variants stay consistent. The runtime view
notation mirrors what `supervisor-mermaid --runtime-deps` emits: solid edges
for lifetime coupling, dotted for spawn order and polled relations, signal
boxes in between.

### Adding a page

1. Add `src/content/docs/<group>/<name>.md` with `title` and `description`
   frontmatter (quote the description if it contains a colon).
2. Reference it by slug in `astro.config.mjs`'s sidebar.
3. `bun run build && node tools/check-links.mjs dist` before committing.

## GitHub Pages

The site deploys through `.github/workflows/deploy.yml` (build, link audit,
artifact upload, Pages deploy). In the repository settings, set **Pages >
Build and deployment > Source** to **GitHub Actions**, then push to `main`.

The site is an org site (`embassy-supervisor.github.io`), served at the
domain root: `site` and `base` are configured at the top of
`astro.config.mjs`. When moving to a project-page repository, change both
there and prefix the root-relative links in `src/content/docs/` and the
`base` in `tools/check-links.mjs` with the new base path.

## License

The site content is part of the embassy-supervisor documentation effort and
follows the crate's licensing: MIT OR Apache-2.0.
