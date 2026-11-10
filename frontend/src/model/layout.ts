export type Slot = {
  id: string;
  hidden: boolean;
  span: 1 | 2;
};

const KEY = "near-flows.layout.v1";

/** Order follows a saved list. New ids append. Missing saves use the given defaults. */
export function normalizeSlots(ids: string[], saved: Slot[] | undefined, wide: string[] = []): Slot[] {
  const known = new Map((saved ?? []).filter((slot) => ids.includes(slot.id)).map((slot) => [slot.id, slot]));
  const rank = new Map((saved ?? []).map((slot, index) => [slot.id, index]));
  const ordered = [...ids].sort((a, b) => (rank.get(a) ?? ids.length) - (rank.get(b) ?? ids.length));
  return ordered.map((id) => {
    const previous = known.get(id);
    if (previous && (previous.span === 1 || previous.span === 2)) {
      return { id, hidden: previous.hidden === true, span: previous.span };
    }
    return { id, hidden: false, span: wide.includes(id) ? 2 : 1 };
  });
}

export function moveSlot(slots: Slot[], id: string, beforeId: string): Slot[] {
  if (id === beforeId) return slots;
  const moving = slots.find((slot) => slot.id === id);
  if (!moving) return slots;
  const rest = slots.filter((slot) => slot.id !== id);
  const index = rest.findIndex((slot) => slot.id === beforeId);
  if (index < 0) return slots;
  return [...rest.slice(0, index), moving, ...rest.slice(index)];
}

export function toggleHidden(slots: Slot[], id: string): Slot[] {
  return slots.map((slot) => (slot.id === id ? { ...slot, hidden: !slot.hidden } : slot));
}

export function toggleSpan(slots: Slot[], id: string): Slot[] {
  return slots.map((slot) => (slot.id === id ? { ...slot, span: slot.span === 1 ? 2 : 1 } : slot));
}

export function loadLayouts(): Record<string, Slot[]> {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as Record<string, Slot[]>;
    return parsed && typeof parsed === "object" ? parsed : {};
  } catch {
    return {};
  }
}

export function saveLayouts(layouts: Record<string, Slot[]>): void {
  localStorage.setItem(KEY, JSON.stringify(layouts));
}
