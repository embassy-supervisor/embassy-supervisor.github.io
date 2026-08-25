// Side panel controller: each of the two rails has three modes, cycled by a
// header button and persisted locally.
//   visible - full rail, content reflows (the default)
//   hover   - collapsed to a thin edge strip; hovering the side reveals the
//             rail over the content until the pointer leaves
//   hidden  - fully collapsed
// The header's right group holds two small buttons, one per panel. Starlight
// hides that group on small screens, so the buttons follow it.

type Mode = 'visible' | 'hover' | 'hidden';

const KEY = 'sup-panels';
const ORDER: Mode[] = ['visible', 'hover', 'hidden'];

const LABELS: Record<Mode, string> = {
  visible: 'visible',
  hover: 'visible on hover',
  hidden: 'hidden',
};

interface State {
  left: Mode;
  right: Mode;
}

function load(): State {
  const fallback: State = { left: 'visible', right: 'visible' };
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return fallback;
    const parsed = JSON.parse(raw) as Partial<State>;
    return {
      left: ORDER.includes(parsed.left!) ? parsed.left! : 'visible',
      right: ORDER.includes(parsed.right!) ? parsed.right! : 'visible',
    };
  } catch {
    return fallback;
  }
}

// A little rail glyph: filled = visible, hatched = hover, outline = hidden.
function panelIcon(side: 'left' | 'right', mode: Mode): string {
  const divider = side === 'left' ? 5.85 : 9.15;
  const w = mode === 'visible' ? 2.4 : 1.2;
  const x = side === 'left' ? 2.35 : 13.75 - 1.1 - w;
  const block =
    mode === 'hidden'
      ? ''
      : `<rect x="${x}" y="3.3" width="${w}" height="9.4" rx="0.6" fill="currentColor"/>`;
  return `<svg width="15" height="16" viewBox="0 0 15 16" aria-hidden="true"><rect x="1.25" y="2" width="12.5" height="12" rx="2" fill="none" stroke="currentColor" stroke-width="1.4"/><path d="M${divider} 2.7v10.6" stroke="currentColor" stroke-width="1.1"/>${block}</svg>`;
}

// The right rail's natural width derives from the content column, so the
// hover reveal must target a measured value rather than a formula.
function captureTocWidth(): void {
  const aside = document.querySelector('aside.right-sidebar-container');
  if (!aside) return;
  const w = Math.round(aside.getBoundingClientRect().width);
  if (w > 0) {
    document.documentElement.style.setProperty('--sup-toc-w', `${w}px`);
  }
}

function button(side: 'left' | 'right', state: State): HTMLButtonElement {
  const btn = document.createElement('button');
  btn.type = 'button';
  btn.className = 'sup-panel-toggle';
  btn.dataset.side = side;
  btn.dataset.mode = state[side];
  const describe = () => {
    btn.innerHTML = panelIcon(side, state[side]);
    btn.setAttribute(
      'aria-label',
      `${side === 'left' ? 'Left' : 'Right'} panel: ${
        LABELS[state[side]]
      }. Click to switch.`,
    );
    btn.title = `Panel mode: ${LABELS[state[side]]}`;
  };
  describe();
  btn.addEventListener('click', () => {
    if (side === 'right') captureTocWidth();
    const mode = ORDER[(ORDER.indexOf(state[side]) + 1) % ORDER.length];
    state[side] = mode;
    btn.dataset.mode = mode;
    document.documentElement.dataset[`panel${side === 'left' ? 'Left' : 'Right'}`] =
      mode;
    localStorage.setItem(KEY, JSON.stringify(state));
    describe();
  });
  return btn;
}

function mount(): void {
  // load() never stomps: ThemeBootstrap already wrote the attributes, and
  // the same state feeds the buttons here.
  const state = load();
  captureTocWidth();
  document.documentElement.dataset.panelLeft = state.left;
  document.documentElement.dataset.panelRight = state.right;

  const group = document.querySelector('header.header .right-group');
  if (!group || group.querySelector('.sup-panel-toggle')) return;

  // Insert right first so the left button lands on the left, matching the
  // rail each one controls.
  group.insertBefore(button('right', state), group.firstElementChild);
  group.insertBefore(button('left', state), group.firstElementChild);
}

document.addEventListener('astro:page-load', mount);
mount();
