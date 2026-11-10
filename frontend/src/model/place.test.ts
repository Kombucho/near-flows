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
});
