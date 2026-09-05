// VS Code-style highlighting for the graph DSL.
//
// The DSL inside supervisor_graph! braces is not valid Rust, so a Lezer
// parse colors it erratically; this tokenizer applies the conventions VS
// Code (Dark+/Light+ with rust-analyzer semantics) shows for these
// declarations: plain clause keywords, teal types and module path segments,
// blue underlined ALL-CAPS statics, gold called functions, green numbers,
// red crate/lifetimes, a blue macro name, and rainbow bracket pairs. Colors
// live in theme.css (.pgt-*) so both site themes stay consistent. Consumed
// by the CodeMirror plugin below and by the graph's declaration popovers.
import {
  Decoration,
  type DecorationSet,
  EditorView,
  ViewPlugin,
  type ViewUpdate,
} from '@codemirror/view';
import { RangeSetBuilder } from '@codemirror/state';

const TOKEN_RE =
  /('(?:static|[a-z_]\w*))|(\b\d[\d_]*(?:\.\d+)?\b)|([A-Za-z_]\w*!)|(\b[A-Za-z_]\w*\b)|([()[\]{}])/g;

function classifyIdent(word: string, after: string): string | null {
  if (word === 'crate' || word === 'self' || word === 'super') return 'pgt-kw';
  if (/^[A-Z][A-Z0-9_]*$/.test(word) && word.length > 1 && !/[a-z]/.test(word)) return 'pgt-const';
  if (/^[A-Z]/.test(word)) return 'pgt-type';
  if (after.startsWith('::')) return 'pgt-mod';
  if (after.startsWith('(')) return 'pgt-fn';
  return null; // clause keywords, markers, path tails: plain text
}

export interface DslToken {
  from: number;
  to: number;
  cls: string;
}

/** Tokenize DSL text into classified spans (unclassified text is plain). */
export function tokenizeDsl(text: string): DslToken[] {
  const out: DslToken[] = [];
  let depth = 0;
  TOKEN_RE.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = TOKEN_RE.exec(text))) {
    const from = m.index;
    const to = from + m[0].length;
    let cls: string | null = null;
    if (m[1]) cls = 'pgt-life';
    else if (m[2]) cls = 'pgt-num';
    else if (m[3]) cls = 'pgt-macro';
    else if (m[4]) cls = classifyIdent(m[4], text.slice(to, to + 2));
    else if (m[5]) {
      // Bracket pair colorization by nesting depth, VS Code style.
      if (m[5] === '(' || m[5] === '[' || m[5] === '{') {
        cls = `pgt-br${depth % 3}`;
        depth++;
      } else {
        depth = Math.max(0, depth - 1);
        cls = `pgt-br${depth % 3}`;
      }
    }
    if (cls) out.push({ from, to, cls });
  }
  return out;
}

/**
 * Extract one item's declaration from DSL source: `node NAME = ...;`,
 * `pool NAME = ...;`, `executor NAME;` or `default executor NAME;`, dedented
 * to its own left margin. Declarations carry no inner semicolons, so the
 * next `;` closes them.
 */
export function extractDecl(src: string, name: string): string | null {
  const ident = name.replace(/[^A-Za-z0-9_]/g, '');
  const re = new RegExp(`(?:^|\\n)([ \\t]*)((?:node|pool|(?:default\\s+)?executor)\\s+${ident}\\b[^;]*;?)`);
  const m = re.exec(src);
  if (!m) return null;
  const indent = m[1];
  return m[2]
    .split('\n')
    .map((l) => (l.startsWith(indent) ? l.slice(indent.length) : l.trimStart()))
    .join('\n')
    .trimEnd();
}

function buildDecorations(view: EditorView): DecorationSet {
  const builder = new RangeSetBuilder<Decoration>();
  for (const t of tokenizeDsl(view.state.doc.toString())) {
    builder.add(t.from, t.to, Decoration.mark({ class: t.cls }));
  }
  return builder.finish();
}

export const dslHighlight = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet;

    constructor(view: EditorView) {
      this.decorations = buildDecorations(view);
    }

    update(u: ViewUpdate) {
      if (u.docChanged) this.decorations = buildDecorations(u.view);
    }
  },
  { decorations: (v) => v.decorations },
);
