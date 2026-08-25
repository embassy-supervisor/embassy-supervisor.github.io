// One-shot link audit over the built site: every internal href must resolve
// to a file in dist/. Run with: node tools/check-links.mjs
import { readdirSync, readFileSync, existsSync, statSync } from 'node:fs';
import { join, resolve } from 'node:path';

const dist = resolve(process.argv[2] ?? 'dist');
const htmlFiles = [];
const walk = (dir) => {
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    if (e.isDirectory()) walk(p);
    else if (e.name.endsWith('.html')) htmlFiles.push(p);
  }
};
walk(dist);

const base = '/';
const hrefs = new Set();
const re = /href="([^"]+)"/g;
for (const f of htmlFiles) {
  const html = readFileSync(f, 'utf8');
  let m;
  while ((m = re.exec(html))) hrefs.add(m[1]);
}

const broken = [];
for (const href of hrefs) {
  if (!href.startsWith(base)) continue; // external or asset-relative
  let path = href.slice(base.length).split('#')[0].split('?')[0];
  if (path === '') path = 'index.html';
  const candidates = [
    join(dist, path),
    join(dist, path, 'index.html'),
    join(dist, path.replace(/\/$/, '') + '.html'),
  ];
  const ok = candidates.some((c) => existsSync(c) && statSync(c).isFile());
  if (!ok) broken.push(href);
}

console.log(`audited ${hrefs.size} hrefs across ${htmlFiles.length} pages`);
if (broken.length) {
  console.log('BROKEN:');
  for (const b of [...broken].sort()) console.log('  ' + b);
  process.exit(1);
}
console.log('all internal links resolve');
