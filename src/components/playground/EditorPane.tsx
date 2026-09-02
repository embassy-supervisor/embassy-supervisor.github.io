// CodeMirror 6 editor for the graph DSL: rust base grammar, parse errors and
// dataflow lints in the gutter, badges for clauses the interpreter degrades.
import { useEffect, useMemo, useRef } from 'react';
import { EditorView, keymap, lineNumbers, highlightActiveLine } from '@codemirror/view';
import { EditorState, Compartment } from '@codemirror/state';
import { defaultKeymap, history, historyKeymap, indentWithTab } from '@codemirror/commands';
import { rust } from '@codemirror/lang-rust';
import { linter, lintGutter, type Diagnostic } from '@codemirror/lint';
import { dslHighlight } from './dslHighlight';
import type { ParseOutcome } from '../../lib/playground/wasm';

interface Props {
  value: string;
  onChange: (v: string) => void;
  parse: ParseOutcome | null;
}

export default function EditorPane({ value, onChange, parse }: Props) {
  const hostRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const parseRef = useRef<ParseOutcome | null>(parse);
  const lintCompartment = useMemo(() => new Compartment(), []);

  parseRef.current = parse;

  useEffect(() => {
    if (!hostRef.current) return;
    const diagSource = (view: EditorView): Diagnostic[] => {
      const p = parseRef.current;
      if (!p) return [];
      const doc = view.state.doc;
      const out: Diagnostic[] = [];
      for (const e of p.errors) {
        const line = doc.line(Math.min(Math.max(e.line, 1), doc.lines));
        out.push({ from: line.from, to: line.to, severity: 'error', message: e.msg });
      }
      for (const l of p.lints) {
        out.push({ from: 0, to: 0, severity: 'warning', message: l });
      }
      return out;
    };
    const view = new EditorView({
      parent: hostRef.current,
      state: EditorState.create({
        doc: value,
        extensions: [
          lineNumbers(),
          history(),
          highlightActiveLine(),
          // rust() supplies indentation and bracket behavior; coloring comes
          // from the DSL tokenizer (a Lezer parse of the not-quite-Rust DSL
          // colors erratically).
          rust(),
          dslHighlight,
          lintGutter(),
          lintCompartment.of(linter(diagSource, { delay: 50 })),
          keymap.of([indentWithTab, ...defaultKeymap, ...historyKeymap]),
          EditorView.updateListener.of((u) => {
            if (u.docChanged) onChange(u.state.doc.toString());
          }),
          EditorView.theme({}, { dark: false }),
        ],
      }),
    });
    viewRef.current = view;
    return () => view.destroy();
    // The view is created once; value/parse updates flow through effects below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // External value change (scenario switch) replaces the document.
  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    const current = view.state.doc.toString();
    if (current !== value) {
      view.dispatch({ changes: { from: 0, to: current.length, insert: value } });
    }
  }, [value]);

  // Refresh diagnostics when a new parse arrives.
  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    view.dispatch({
      effects: lintCompartment.reconfigure(
        linter(
          (v) => {
            const p = parseRef.current;
            if (!p) return [];
            const doc = v.state.doc;
            const out: Diagnostic[] = [];
            for (const e of p.errors) {
              const line = doc.line(Math.min(Math.max(e.line, 1), doc.lines));
              out.push({ from: line.from, to: line.to, severity: 'error', message: e.msg });
            }
            for (const l of p.lints) {
              out.push({ from: 0, to: 0, severity: 'warning', message: l });
            }
            return out;
          },
          { delay: 50 },
        ),
      ),
    });
  }, [parse, lintCompartment]);

  return (
    <div className="pg-editor-wrap">
      <div className="pg-pane-title">
        <span>supervisor_graph!</span>
        {parse && !parse.ok && <span className="pg-status-err">{parse.errors.length} {parse.errors.length === 1 ? 'error' : 'errors'}</span>}
        {parse?.ok && <span className="pg-status-ok">parsed</span>}
      </div>
      <div ref={hostRef} className="pg-cm-host" />
      {parse && parse.badges.length > 0 && (
        <details className="pg-badges">
          <summary>{parse.badges.length} clause notes</summary>
          <ul>
            {parse.badges.map((b, i) => (
              <li key={i}>
                <code>
                  {b.item} {b.clause}
                </code>{' '}
                {b.note}
              </li>
            ))}
          </ul>
        </details>
      )}
    </div>
  );
}
