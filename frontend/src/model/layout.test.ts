import { describe, expect, it } from "vitest";
import { moveSlot, normalizeSlots, toggleHidden, toggleSpan } from "./layout";

describe("card layout", () => {
  it("keeps a saved order and appends a new card", () => {
    const saved = [
      { id: "activity", hidden: false, span: 1 as const },
      { id: "hero", hidden: true, span: 2 as const },
    ];
    expect(normalizeSlots(["hero", "holdings", "activity"], saved, ["hero"]).map((slot) => slot.id)).toEqual([
      "activity",
      "hero",
      "holdings",
    ]);
    expect(normalizeSlots(["hero", "holdings", "activity"], saved, ["hero"]).find((slot) => slot.id === "hero")).toEqual({
      id: "hero",
      hidden: true,
      span: 2,
    });
    expect(normalizeSlots(["hero", "holdings", "activity"], saved).find((slot) => slot.id === "holdings")?.hidden).toBe(false);
  });

  it("reorders, hides, and resizes without dropping a card", () => {
    const start = normalizeSlots(["hero", "holdings", "activity"], undefined, ["hero"]);
    const moved = moveSlot(start, "activity", "hero");
    expect(moved.map((slot) => slot.id)).toEqual(["activity", "hero", "holdings"]);
    const hidden = toggleHidden(moved, "holdings");
    expect(hidden.find((slot) => slot.id === "holdings")?.hidden).toBe(true);
    expect(hidden).toHaveLength(3);
    expect(toggleSpan(hidden, "hero").find((slot) => slot.id === "hero")?.span).toBe(1);
  });
});
