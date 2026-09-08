import { copyFile } from 'node:fs/promises';

export function sitemapAlias() {
  return {
    name: 'sitemap-alias',
    hooks: {
      'astro:build:done': async ({ dir, logger }) => {
        try {
          await copyFile(
            new URL('sitemap-index.xml', dir),
            new URL('sitemap.xml', dir),
          );
          logger.info('`sitemap.xml` copied from `sitemap-index.xml`');
        } catch (err) {
          // Not fatal: the index stays reachable at its own URL, and
          // robots.txt points there.
          logger.warn(`no \`sitemap.xml\` alias: ${err.message}`);
        }
      },
    },
  };
}
