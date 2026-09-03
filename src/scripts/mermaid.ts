// Diagram renderer: takes ```mermaid fences (as emitted for graphs declared
// with supervisor-mermaid or written by hand), renders them with the site's
// light and dark "instrument panel" themes, and adds the draw-in / marching
// motion. One controller per page session; re-renders on theme flips and
// view transitions.

import type Mermaid from 'mermaid';

type Variant = 'dark' | 'light';

interface Figure {
  figure: HTMLElement;
  canvas: HTMLElement;
  source: string;
  svg?: SVGSVGElement;
}

const PREAMBLE: Record<Variant, string> = {
  dark: `
classDef task fill:#131a30,stroke:#22d3ee,stroke-width:1.8px,color:#e2e8f0;
classDef pool fill:#131a30,stroke:#f472b6,stroke-width:1.8px,color:#e2e8f0;
classDef provider fill:#131a30,stroke:#a3e635,stroke-width:1.8px,color:#e2e8f0;
classDef paused fill:#131a30,stroke:#fb7185,stroke-width:1.8px,stroke-dasharray:5 3,color:#e2e8f0;
classDef disabled fill:#0d1322,stroke:#546180,stroke-width:1.5px,stroke-dasharray:2 4,color:#8ea0bf;
classDef signal fill:#0d1322,stroke:#a78bfa,stroke-width:1.3px,color:#c4b5fd;
classDef resource fill:#0d1322,stroke:#38bdf8,stroke-width:1.3px,color:#a5d8f3;
`,
  light: `
classDef task fill:#ffffff,stroke:#0e7490,stroke-width:1.8px,color:#0f172a;
classDef pool fill:#ffffff,stroke:#db2777,stroke-width:1.8px,color:#0f172a;
classDef provider fill:#ffffff,stroke:#4d7c0f,stroke-width:1.8px,color:#0f172a;
classDef paused fill:#ffffff,stroke:#be123c,stroke-width:1.8px,stroke-dasharray:5 3,color:#0f172a;
classDef disabled fill:#eef1f8,stroke:#94a3b8,stroke-width:1.5px,stroke-dasharray:2 4,color:#64748b;
classDef signal fill:#f5f3fe,stroke:#6d28d9,stroke-width:1.3px,color:#4c1d95;
classDef resource fill:#eff7fd,stroke:#0369a1,stroke-width:1.3px,color:#0c4a6e;
`,
};

const THEME_VARS: Record<Variant, Record<string, string>> = {
  dark: {
    background: 'transparent',
    primaryColor: '#131a30',
    primaryBorderColor: '#22d3ee',
    primaryTextColor: '#e2e8f0',
    lineColor: '#a78bfa',
    textColor: '#b6c2d9',
    edgeLabelBackground: '#0a0e1c',
    clusterBkg: '#0d132280',
    clusterBorder: '#26304d',
    titleColor: '#e2e8f0',
    errorCode: '#fb7185',
    errorTextColor: '#e2e8f0',
    errorBkgColor: '#2a1220',
  },
  light: {
    background: 'transparent',
    primaryColor: '#ffffff',
    primaryBorderColor: '#0e7490',
    primaryTextColor: '#0f172a',
    lineColor: '#6d28d9',
    textColor: '#37415a',
    edgeLabelBackground: '#f7f8fc',
    clusterBkg: '#e9edf8',
    clusterBorder: '#ccd5ea',
    titleColor: '#0f172a',
    errorCode: '#be123c',
    errorTextColor: '#1a2233',
    errorBkgColor: '#fde8ec',
  },
};

const FONT = '"IBM Plex Mono", ui-monospace, monospace';

class Diagrams {
  private figures: Figure[] = [];
  private mermaid: Mermaid;
  private seq = 0;
  private variant: Variant = 'dark';
  private themeObserver: MutationObserver;

  constructor(mermaid: Mermaid) {
    this.mermaid = mermaid;
    this.initialize(
      document.documentElement.dataset.theme === 'light' ? 'light' : 'dark',
    );
    this.scan();

    document.addEventListener('astro:page-load', () => this.scan());

    // Re-render every diagram when the color scheme flips.
    this.themeObserver = new MutationObserver(() => {
      const theme =
        document.documentElement.dataset.theme === 'light' ? 'light' : 'dark';
      if (theme !== this.variant) {
        this.initialize(theme);
        for (const f of this.figures) void this.render(f, theme);
      }
    });
    this.themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme'],
    });
  }

  private initialize(variant: Variant) {
    this.variant = variant;
    this.mermaid.initialize({
      startOnLoad: false,
      theme: 'base',
      darkMode: variant === 'dark',
      securityLevel: 'strict',
      fontFamily: FONT,
      themeVariables: { ...THEME_VARS[variant], fontSize: '12px' },
      flowchart: {
        htmlLabels: true,
        curve: 'basis',
        nodeSpacing: 46,
        rankSpacing: 58,
        padding: 12,
      },
      state: {
        padding: 12,
      },
    });
  }

  /** Render every un-rendered mermaid fence on the page. */
  scan() {
    const blocks = document.querySelectorAll<Element>(
      'pre[data-language="mermaid"], pre > code.language-mermaid',
    );
    for (const block of blocks) {
      const pre = block.matches('pre')
        ? (block as HTMLPreElement)
        : (block.parentElement as HTMLPreElement | null);
      if (!pre || pre.dataset.rendered === '1') continue;
      pre.dataset.rendered = '1';
      const figure = document.createElement('figure');
      figure.className = 'graph';
      figure.setAttribute('data-variant', this.variant);
      const canvas = document.createElement('div');
      canvas.className = 'graph-canvas';
      figure.appendChild(canvas);
      // newlines in textContent; rebuild the source line by line.
      const code = pre.querySelector('code') ?? pre;
      const lines = code.querySelectorAll(':scope > div, :scope > span');
      const source = lines.length
        ? Array.from(lines, (l) => l.textContent ?? '').join('\n')
        : (code.textContent ?? '');
      const f: Figure = { figure, canvas, source };
      this.figures.push(f);
      pre.replaceWith(figure);
      void this.render(f, this.variant);
    }
  }

  private async render(f: Figure, variant: Variant) {
    const id = `sup-graph-${this.seq++}`;
    // classDefs only parse after the diagram type line, and only flowcharts
    // take them; state and sequence diagrams keep the base theme alone.
    const isFlowchart = /^\s*(flowchart|graph)\b/m.test(f.source);
    const source = isFlowchart
      ? f.source + '\n' + PREAMBLE[variant]
      : f.source;
    try {
      const { svg } = await this.mermaid.render(id, source);
      f.canvas.innerHTML = svg;
      const svgEl = f.canvas.querySelector('svg');
      if (svgEl) {
        svgEl.removeAttribute('height');
        const natural = svgEl.viewBox?.baseVal?.width;
        if (natural) f.figure.style.setProperty('--graph-natural-w', `${natural}px`);
        f.figure.setAttribute('data-variant', variant);
        f.svg = svgEl;
        this.animate(svgEl);
        this.wire(f);
      }
    } catch (err) {
      const message = String((err as Error)?.message ?? err)
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;');
      f.canvas.innerHTML = `<div class="graph-error"><strong>Diagram source rejected</strong><pre>${message}</pre></div>`;
    }
  }

  /** Draw-in for solid edges, a slow dash march for dotted ones. */
  private animate(svg: SVGSVGElement) {
    if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) return;
    const edges = svg.querySelectorAll<SVGPathElement>(
      '.edgePaths path.flowchart-link, .edgePath path.flowchart-link',
    );
    let i = 0;
    for (const edge of edges) {
      const style = edge.getAttribute('style') ?? '';
      const dotted = /stroke-dasharray:\s*[\d.]+\s*[, ]/.test(style);
      if (dotted) {
        const match = style.match(/stroke-dasharray:\s*([\d. ,]+)/);
        edge.style.strokeDasharray = match?.[1]?.trim() ?? '4 4';
        edge.style.animation = `graph-march 1.6s linear ${i * 0.12}s infinite`;
      } else {
        const len = edge.getTotalLength();
        edge.style.strokeDasharray = `${len} ${len}`;
        edge.style.strokeDashoffset = `${len}`;
        edge.style.transition = 'none';
        // Force a style flush, then animate the draw-in.
        edge.getBoundingClientRect();
        edge.style.transition = `stroke-dashoffset .9s cubic-bezier(.22,.61,.36,1) ${i * 0.09}s`;
        edge.style.strokeDashoffset = '0';
        window.setTimeout(() => {
          edge.style.strokeDasharray = '';
          edge.style.transition = '';
        }, 1400 + i * 90);
      }
      i++;
    }
  }

  /**
   * Hover focus: hovering a node or an edge highlights its neighbourhood and
   * dims the rest of the figure. Mermaid ids are `<prefix>-flowchart-<id>-N`
   * on nodes and `L_<from>_<to>_<i>` on both edge paths and edge labels.
   */
  private wire(f: Figure) {
    const svg = f.svg;
    if (!svg) return;

    const nodeById = new Map<string, SVGGElement>();
    for (const n of svg.querySelectorAll<SVGGElement>('.node')) {
      const m = (n.id ?? '').match(/-(\w+)-\d+$/);
      if (m) nodeById.set(m[1], n);
    }
    if (nodeById.size === 0) return;
    const ids = [...nodeById.keys()];

    // Mermaid encodes an edge id as L_<from>_<to>_<i>, joining the two node
    // ids with a "_" separator that node ids may themselves contain (e.g.
    // L_NET_NET_STACK_0 for NET -> NET_STACK). Try every split, skipping the
    // separator, and take the first that names two real nodes.
    const parseEdge = (dataId: string): [string, string] | null => {
      const core = dataId.slice(2).replace(/_\d+$/, '');
      for (let i = 1; i < core.length - 1; i++) {
        const a = core.slice(0, i);
        const b = core.slice(i + 1);
        if (nodeById.has(a) && nodeById.has(b)) return [a, b];
      }
      return null;
    };

    interface Edge {
      id: string;
      from: string;
      to: string;
      path: Element;
      label: Element | null;
    }
    const edges: Edge[] = [];
    for (const path of svg.querySelectorAll('[data-id^="L_"]')) {
      if (!path.closest('.edgePaths')) continue;
      const dataId = path.getAttribute('data-id')!;
      const pair = parseEdge(dataId);
      if (!pair) continue;
      const label = svg.querySelector(`.edgeLabels .label[data-id="${dataId}"]`);
      edges.push({ id: dataId, from: pair[0], to: pair[1], path, label });
    }
    if (edges.length === 0) return;

    const neighbors = new Map<string, Set<string>>(ids.map((i) => [i, new Set()]));
    for (const e of edges) {
      neighbors.get(e.from)!.add(e.to);
      neighbors.get(e.to)!.add(e.from);
    }

    const activate = (focus: Set<string>, hot: Set<Edge>) => {
      svg.classList.add('focus-mode');
      for (const [id, node] of nodeById) {
        node.classList.toggle('f-on', focus.has(id));
      }
      for (const e of edges) {
        const on = hot.has(e);
        e.path.classList.toggle('f-hot', on);
        e.label?.classList.toggle('f-hot', on);
      }
    };

    const clear = () => {
      svg.classList.remove('focus-mode');
      for (const [, node] of nodeById) node.classList.remove('f-on');
      for (const e of edges) {
        e.path.classList.remove('f-hot');
        e.label?.classList.remove('f-hot');
      }
    };

    for (const [id, node] of nodeById) {
      node.addEventListener('mouseenter', () => {
        const focus = new Set([id, ...neighbors.get(id)!]);
        activate(focus, new Set(edges.filter((e) => e.from === id || e.to === id)));
      });
      node.addEventListener('mouseleave', clear);
    }
    for (const e of edges) {
      e.path.addEventListener('mouseenter', () =>
        activate(new Set([e.from, e.to]), new Set([e])),
      );
      e.path.addEventListener('mouseleave', clear);
    }
  }
}

declare global {
  interface Window {
    __supervisorDiagrams?: Diagrams;
  }
}

async function boot() {
  if (window.__supervisorDiagrams) return;
  try {
    const mermaid = (await import('mermaid')).default;
    // Optional layout algorithms, opted into per diagram (frontmatter
    // `config: { layout: elk }`). Dynamic like the mermaid import above:
    // elk must stay out of the initial bundle of pages without diagrams.
    const elkLayouts = (await import('@mermaid-js/layout-elk')).default;
    mermaid.registerLayoutLoaders(elkLayouts);
    // Mermaid measures label boxes with the configured font family; rendering
    // before the webfonts land measures with fallback metrics and clips
    // multi-line labels. Wait for the fonts (bounded, in case none load).
    await Promise.race([
      document.fonts.ready,
      (() => {
        const { promise, resolve } = Promise.withResolvers<void>();
        window.setTimeout(resolve, 2500);
        return promise;
      })(),
    ]);
    window.__supervisorDiagrams = new Diagrams(mermaid);
  } catch (err) {
    // In dev, this is usually a stale Vite optimized-deps URL: restart
    // `astro dev` (or hard-refresh) and the diagram chunks reload cleanly.
    console.error(
      'embassy-supervisor-site: diagram renderer failed to load; restart the dev server if this persists.',
      err,
    );
  }
}
void boot();
