/**
 * Wrap every markdown table in its own scroll container.
 */
export function rehypeTableScroll() {
  const wrap = (node) => {
    if (!Array.isArray(node.children)) return;
    for (const child of node.children) wrap(child);
    node.children = node.children.map((child) =>
      child.type === 'element' && child.tagName === 'table'
        ? {
            type: 'element',
            tagName: 'div',
            properties: { className: ['table-scroll'], tabIndex: 0 },
            children: [child],
          }
        : child,
    );
  };
  return (tree) => {
    wrap(tree);
  };
}
