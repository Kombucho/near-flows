import { describe, expect, it } from "vitest";
import { boardDocument } from "../model/draft";
import { exportLocalDraft } from "../model/flow";
import { DEMO_ACCOUNT, DEMO_CARD_IDS, DEMO_KIND, cardFace, draftContainsDemo } from "./fixtures";

describe("labelled demo account", () => {
  it("tags every demo card and names the keeper", () => {
    for (const id of DEMO_CARD_IDS) {
      expect(cardFace(true, id).tag, id).toBe("Demo");
    }
    expect(cardFace(true, "strategies.keeper").primary).toBe("Demo keeper");
    expect(cardFace(true, "portfolio.keeper").primary).toBe("Demo keeper");
    expect(DEMO_ACCOUNT.kind).toBe(DEMO_KIND);
    expect(JSON.stringify(DEMO_ACCOUNT.activity)).not.toContain('"sent"');
  });

  it("restores unknown when demo is off and stays out of a real flow", () => {
    for (const id of DEMO_CARD_IDS) {
      const face = cardFace(false, id);
      expect(face.tag, id).toBeNull();
      expect(face.primary === "Unavailable" || face.primary === "unknown", id).toBe(true);
      expect(face.primary).not.toBe(String(DEMO_ACCOUNT.totalUsd));
    }
    const flow = boardDocument("local-named");
    const exported = exportLocalDraft(flow);
    expect(draftContainsDemo(exported)).toBe(false);
    expect(draftContainsDemo(JSON.stringify({ demo: true, totalUsd: DEMO_ACCOUNT.totalUsd, kind: DEMO_KIND }))).toBe(true);
  });
});
