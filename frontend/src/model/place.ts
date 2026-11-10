/** Card size the canvas lays out around. Wider than the seed coordinates, so those coordinates overlap. */
export const NODE_W = 300;
export const NODE_H = 292;
export const GAP_X = 80;
export const GAP_Y = 48;

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
  for (const [column, list] of columns) {
    list.forEach((node, row) => {
      placed.push({
        id: node.id,
        x: column * (NODE_W + GAP_X),
        y: row * (NODE_H + GAP_Y),
      });
    });
  }
  return placed;
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
