import { describe, expect, it } from "vitest";
import { boardDocument } from "./draft";
import { boxesCollide, layoutGraph, NODE_H, NODE_W } from "./place";

describe("flow layout", () => {
  it("separates the seeded draft, which collides once cards are full size", () => {
    const nodes = boardDocument("v").nodes;
    expect(boxesCollide(nodes)).toBe(true);
    const placed = layoutGraph(nodes);
    const laid = nodes.map((node) => ({ position: placed.find((item) => item.id === node.id)! }));
    expect(boxesCollide(laid)).toBe(false);
    const sells = placed.filter((item) => nodes.find((node) => node.id === item.id)?.title === "Sell ⅓");
    expect(new Set(sells.map((item) => item.x)).size).toBe(1);
    expect(sells[1].y - sells[0].y).toBeGreaterThanOrEqual(NODE_H);
    const nvda = placed.find((item) => item.id === "nvda")!;
    expect(sells[0].x - nvda.x).toBeGreaterThanOrEqual(NODE_W);
  });

  it("sends every route to the right, so an edge never has to turn back", () => {
    const nodes = boardDocument("v").nodes;
    const at = new Map(layoutGraph(nodes).map((item) => [item.id, item]));
    for (const node of nodes) {
      for (const route of node.routes) {
        const target = nodes.find((item) => item.index === route.to);
        expect(target).toBeTruthy();
        expect(at.get(target!.id)!.x).toBeGreaterThan(at.get(node.id)!.x);
      }
    }
  });
});
