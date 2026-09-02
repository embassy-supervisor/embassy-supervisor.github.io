// Whether the consent banner and GA4 are live for this build. Both the
// Analytics component and the footer's "Cookie preferences" link key off
// this: a link that opens a modal no page ever loaded is worse than no link.
//
// `astro dev` is not PROD, and a fork building for another `site` fails the
// host check, so neither ships outside the published site.
export const SITE_HOST = 'embassy-supervisor.github.io';

export function consentEnabled(site: URL | undefined): boolean {
  return import.meta.env.PROD && new URL(site ?? 'http://localhost').hostname === SITE_HOST;
}
