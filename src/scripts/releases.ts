// The landing page's release list, filled in the browser from the GitHub
// releases API and remembered in local storage.
//
const API =
  'https://api.github.com/repos/cedrivard/embassy-supervisor/releases?per_page=100';
const KEY = 'sup-releases';
/** Bumped when the stored shape or the request changes, so an older copy
 *  is refetched instead of trusted. */
const CACHE_VERSION = 2;
/** How long a cached list is shown without asking GitHub again. */
const FRESH_MS = 30 * 60_000;
/** Auto-scroll speed, CSS pixels per second. */
const SPEED = 14;

interface Release {
  /** Crate the release belongs to, e.g. `embassy-supervisor-tools`. */
  crate: string;
  /** Version without the tag's `v`, e.g. `0.5.0`. */
  version: string;
  /** The release page on GitHub. */
  url: string;
  /** Publication timestamp, for `<time datetime>`. */
  iso: string;
  /** The opening paragraph of the release notes, as plain text with
   *  backticks kept for the code spans; empty when the notes are. */
  note: string;
}

interface Cache {
  v: number;
  at: number;
  etag: string | null;
  items: Release[];
}

interface ApiRelease {
  tag_name: string;
  html_url: string;
  published_at: string | null;
  draft: boolean;
  body: string | null;
}

// The notes open with a summary paragraph (hard-wrapped, sometimes a list
// item) before the `### Added` sections; that paragraph is the line. Links
// keep their text, emphasis markers go, code spans stay for `render`.
function firstParagraph(body: string | null): string {
  if (!body) return '';
  const para: string[] = [];
  for (const raw of body.split(/\r?\n/)) {
    const line = raw.trim();
    if (para.length === 0) {
      if (!line || line.startsWith('#')) continue;
    } else if (!line || line.startsWith('#')) {
      break;
    }
    para.push(line.replace(/^[-*]\s+/, ''));
  }
  return para
    .join(' ')
    .replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/(\*\*|__)(.+?)\1/g, '$2')
    .replace(/(^|\s)[*_]([^*_]+)[*_](?=[\s.,;:]|$)/g, '$1$2');
}

function load(): Cache | null {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return null;
    const c = JSON.parse(raw) as Cache;
    return c.v === CACHE_VERSION && Array.isArray(c.items) && typeof c.at === 'number'
      ? c
      : null;
  } catch {
    return null;
  }
}

function store(c: Cache): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(c));
  } catch {
    // Private mode or a full quota: the list still renders, it is just not
    // remembered.
  }
}

// Release names are written by hand and the `v` comes and goes, so the tag
// is the source: `embassy-supervisor-tools-v0.5.0`, and the root crate's
// early `v0.6.0` with no crate prefix.
const TAG = /^(?:(.+?)-)?v?(\d+\.\d+.*)$/;

function parse(list: ApiRelease[]): Release[] {
  const out: Release[] = [];
  for (const r of list) {
    if (r.draft || !r.published_at) continue;
    const m = TAG.exec(r.tag_name);
    if (!m) continue;
    out.push({
      crate: m[1] ?? 'embassy-supervisor',
      version: m[2],
      url: r.html_url,
      iso: r.published_at,
      note: firstParagraph(r.body),
    });
  }
  // The API orders by creation; publication is what the date column shows.
  out.sort((a, b) => (a.iso < b.iso ? 1 : a.iso > b.iso ? -1 : 0));
  return out;
}

// Formatted by hand rather than through `Intl`: CLDR abbreviates September
// as either "Sep" or "Sept" depending on the browser's locale data, and the
// ticker should read the same everywhere.
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

function shortDate(iso: string, now: Date): string {
  const d = new Date(iso);
  const day = `${MONTHS[d.getMonth()]} ${d.getDate()}`;
  return d.getFullYear() === now.getFullYear() ? day : `${day}, ${d.getFullYear()}`;
}

async function refresh(cached: Cache | null): Promise<Cache | null> {
  const headers: Record<string, string> = { accept: 'application/vnd.github+json' };
  if (cached?.etag) headers['if-none-match'] = cached.etag;
  let res: Response;
  try {
    res = await fetch(API, { headers, signal: AbortSignal.timeout(8_000) });
  } catch {
    return null;
  }
  if (res.status === 304 && cached) {
    return { ...cached, at: Date.now() };
  }
  if (!res.ok) return null;
  const items = parse((await res.json()) as ApiRelease[]);
  return { v: CACHE_VERSION, at: Date.now(), etag: res.headers.get('etag'), items };
}

/** The note's text, with backtick spans as `<code>`. */
function noteNodes(note: string): (string | HTMLElement)[] {
  const out: (string | HTMLElement)[] = [];
  const parts = note.split('`');
  parts.forEach((part, i) => {
    if (!part) return;
    if (i % 2 === 0) {
      out.push(part);
    } else {
      const c = document.createElement('code');
      c.textContent = part;
      out.push(c);
    }
  });
  return out;
}

function run(items: Release[], now: Date, copy: boolean): HTMLUListElement {
  const ul = document.createElement('ul');
  ul.className = 'rel-run';
  if (copy) ul.setAttribute('aria-hidden', 'true');
  for (const r of items) {
    const li = document.createElement('li');
    const a = document.createElement('a');
    a.href = r.url;
    if (copy) a.tabIndex = -1;
    const line = document.createElement('span');
    line.className = 'rel-line';
    const crate = document.createElement('span');
    crate.className = 'rel-crate';
    crate.textContent = r.crate;
    const ver = document.createElement('span');
    ver.className = 'rel-ver';
    ver.textContent = r.version;
    const t = document.createElement('time');
    t.className = 'rel-date';
    t.dateTime = r.iso;
    t.textContent = shortDate(r.iso, now);
    line.append(crate, ver, t);
    a.append(line);
    if (r.note) {
      const note = document.createElement('span');
      note.className = 'rel-note';
      note.append(...noteNodes(r.note));
      a.append(note);
    }
    li.append(a);
    ul.append(li);
  }
  return ul;
}

/**
 * Drives the list. The motion is a CSS animation on the track (see
 * `.rel-track` in theme.css); this sets its duration from the run's height
 * so the pace is `SPEED`, and turns the wheel into a seek on that
 * animation, so the list moves by hand whether it is running, held under
 * the pointer, or still under a reduced-motion setting. One driver per
 * list.
 */
function drive(mask: HTMLElement, track: HTMLElement): void {
  // The track is two copies of the run; one run is half its height.
  const runHeight = () => track.offsetHeight / 2;
  const pace = () => {
    const runH = runHeight();
    mask.classList.toggle('rel-static', runH <= mask.clientHeight);
    track.style.setProperty('--rel-dur', `${runH / SPEED}s`);
  };
  new ResizeObserver(pace).observe(track);

  mask.addEventListener(
    'wheel',
    (e) => {
      const anim = track.getAnimations()[0];
      if (!anim || mask.classList.contains('rel-static')) return;
      e.preventDefault();
      // Firefox reports lines, occasionally pages; the others pixels.
      const px =
        e.deltaMode === 1
          ? e.deltaY * 16
          : e.deltaMode === 2
            ? e.deltaY * mask.clientHeight
            : e.deltaY;
      const dur = (runHeight() / SPEED) * 1000;
      const at = typeof anim.currentTime === 'number' ? anim.currentTime : 0;
      const t = at + (px / SPEED) * 1000;
      // Wrap within one run: the animation repeats, so any point of it is
      // a point of the seamless loop.
      anim.currentTime = ((t % dur) + dur) % dur;
    },
    { passive: false },
  );
}

function render(root: HTMLElement, items: Release[]): void {
  const mask = root.querySelector<HTMLElement>('.rel-mask');
  const track = root.querySelector<HTMLElement>('.rel-track');
  if (!mask || !track || items.length === 0) return;
  const now = new Date();
  // Two copies of the run: see `drive`.
  track.replaceChildren(run(items, now, false), run(items, now, true));
  if (!mask.dataset.driven) {
    mask.dataset.driven = '1';
    drive(mask, track);
  }
}

function mount(): void {
  const root = document.querySelector<HTMLElement>('[data-releases]');
  if (!root) return;

  const cached = load();
  if (cached) render(root, cached.items);
  if (cached && Date.now() - cached.at < FRESH_MS) return;

  void refresh(cached).then((fresh) => {
    if (!fresh) return;
    store(fresh);
    if (fresh.items !== cached?.items) render(root, fresh.items);
  });
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', mount, { once: true });
} else {
  mount();
}
