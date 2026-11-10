/** Trading-card size the canvas lays out around. The seed coordinates are tighter, so they overlap. */
export const NODE_W = 252;
export const NODE_H = 352;
export const GAP_X = 88;
export const GAP_Y = 56;

export type PlaceNode = { id: string; index: number; routes: { to: number }[] };

/** Left to right by dependency. Nodes that share a column stack with a full card of space between them. */
export function layoutGraph(nodes: PlaceNode[]): { id: string; x: number; y: number }[] {
  const rank = new Map<string, number>();
  const visiting = new Set<string>();
  function rankOf(node: PlaceNode): number {
    const known = rank.get(node.id);
    if (known != null) return known;
    if (visiting.has(node.id)) return 0;
    visiting.add(node.id);
    let best = 0;
    for (const source of nodes) {
      if (source.routes.some((route) => route.to === node.index)) best = Math.max(best, rankOf(source) + 1);
    }
    visiting.delete(node.id);
    rank.set(node.id, best);
    return best;
  }
  for (const node of nodes) rankOf(node);
  const columns = new Map<number, PlaceNode[]>();
  for (const node of [...nodes].sort((a, b) => a.index - b.index)) {
    const column = rank.get(node.id) ?? 0;
    const list = columns.get(column) ?? [];
    list.push(node);
    columns.set(column, list);
  }
  const placed: { id: string; x: number; y: number }[] = [];
  const rowOf = new Map<string, number>();
  const byIndex = new Map(nodes.map((node) => [node.index, node]));
  const maxColumn = Math.max(0, ...columns.keys());
  for (let column = 0; column <= maxColumn; column += 1) {
    const list = columns.get(column) ?? [];
    const ordered = [...list].sort((a, b) => {
      const delta = bary(a) - bary(b);
      return delta !== 0 ? delta : a.index - b.index;
    });
    ordered.forEach((node, row) => {
      rowOf.set(node.id, row);
      placed.push({
        id: node.id,
        x: column * (NODE_W + GAP_X),
        y: row * (NODE_H + GAP_Y),
      });
    });
  }
  return placed;

  function bary(node: PlaceNode): number {
    const preds = nodes.filter((source) => source.routes.some((route) => route.to === node.index) && byIndex.has(source.index));
    const rows = preds.map((source) => rowOf.get(source.id)).filter((row): row is number => row != null);
    if (!rows.length) return node.index;
    return rows.reduce((sum, row) => sum + row, 0) / rows.length;
  }
}

export function boxesCollide(nodes: { position: { x: number; y: number } }[]): boolean {
  for (let i = 0; i < nodes.length; i += 1) {
    for (let j = i + 1; j < nodes.length; j += 1) {
      const a = nodes[i].position;
      const b = nodes[j].position;
      const overlapX = Math.abs(a.x - b.x) < NODE_W;
      const overlapY = Math.abs(a.y - b.y) < NODE_H;
      if (overlapX && overlapY) return true;
    }
  }
  return false;
}
