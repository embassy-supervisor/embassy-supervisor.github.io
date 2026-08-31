// @ts-check
import { defineConfig } from 'astro/config';
import react from '@astrojs/react';
import starlight from '@astrojs/starlight';

// GitHub Pages org site: served at the domain root. Change both when the
// site moves to a custom domain or a project-page repository.
const SITE = 'https://embassy-supervisor.github.io';
const BASE = '/';

export default defineConfig({
  site: SITE,
  base: BASE,
  trailingSlash: 'ignore',
  // mermaid is only ever reached through a dynamic import chain, so the dev
  // scanner can miss it; pinning it here keeps its optimized-deps URL valid
  // for the whole dev session instead of 504ing after a re-optimize.
  vite: {
    optimizeDeps: {
      include: ['mermaid'],
    },
  },
  integrations: [
    // Used only by the playground island; docs pages ship no React.
    react(),
    starlight({
      title: 'embassy-supervisor',
      description:
        'Run-time supervision for bare-metal async Rust: declare your task graph, and get ordered bring-up, gated spawning, runtime control, verified dataflow and tracing for it.',
      logo: {
        src: './src/assets/logo.svg',
        replacesTitle: false,
      },
      social: [
        {
          icon: 'github',
          label: 'GitHub',
          href: 'https://github.com/cedrivard/embassy-supervisor',
        },
      ],
      favicon: '/favicon.svg',
      // Starlight emits og:title/description/url and twitter:card per page;
      // only the card image is site-supplied.
      head: [
        {
          tag: 'meta',
          attrs: { property: 'og:image', content: `${SITE}${BASE}og.png` },
        },
        {
          tag: 'meta',
          attrs: { property: 'og:image:width', content: '1200' },
        },
        {
          tag: 'meta',
          attrs: { property: 'og:image:height', content: '630' },
        },
        {
          tag: 'meta',
          attrs: {
            property: 'og:image:alt',
            content:
              'embassy-supervisor: run-time supervision for bare-metal async Rust',
          },
        },
      ],
      customCss: ['./src/styles/theme.css'],
      components: {
        Head: './src/components/Head.astro',
      },
      sidebar: [
        {
          label: 'Start',
          items: [
            { label: 'What it is', slug: 'getting-started' },
            { label: 'Installation', slug: 'getting-started/install' },
            { label: 'Your first graph', slug: 'getting-started/first-graph' },
          ],
        },
        {
          label: 'Concepts',
          items: [
            { label: 'The model', slug: 'concepts/model' },
            { label: 'Declaring the graph', slug: 'concepts/dsl' },
            { label: 'Writing supervised tasks', slug: 'concepts/tasks' },
            { label: 'Lifecycle and modes', slug: 'concepts/lifecycle' },
            { label: 'Resources', slug: 'concepts/resources' },
            { label: 'Dependencies and gating', slug: 'concepts/dependencies' },
            { label: 'Dataflow', slug: 'concepts/dataflow' },
            { label: 'Gated reads and leases', slug: 'concepts/data-deps' },
            { label: 'Elastic pools', slug: 'concepts/pools' },
            { label: 'Executors and cores', slug: 'concepts/placement' },
            { label: 'Runtime control', slug: 'concepts/control' },
            { label: 'Health monitoring', slug: 'concepts/health' },
            { label: 'Tracing and profiling', slug: 'concepts/trace' },
            { label: 'Fragments and sub-graphs', slug: 'concepts/composition' },
            { label: 'Heap and state', slug: 'concepts/memory' },
          ],
        },
        {
          label: 'Guides',
          items: [
            { label: 'Pattern gallery', slug: 'guides/patterns' },
            { label: 'Testing on your desktop', slug: 'guides/testing' },
            { label: 'Diagram and lint tools', slug: 'guides/tools' },
            { label: 'The reference firmware', slug: 'guides/demo-firmware' },
          ],
        },
        {
          label: 'Playground',
          items: [
            { label: 'How it works', slug: 'guides/playground-notes' },
            { label: 'The scenarios', slug: 'guides/playground-scenarios' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { label: 'Feature flags', slug: 'reference/features' },
            { label: 'Errors and limits', slug: 'reference/errors' },
            { label: 'Glossary', slug: 'reference/glossary' },
            { label: 'Learn more', slug: 'reference/learn-more' },
          ],
        },
      ],
    }),
  ],
});
