import { test } from "node:test";
import assert from "node:assert/strict";
import { wrap } from "./chain.ts";

test("a vault call is the old method's args wrapped under its name", () => {
  assert.deepEqual(wrap("step", { flow_id: 2, node: 0 }), { step: { flow_id: 2, node: 0 } });
  assert.equal(JSON.stringify(wrap("step_hidden", { flow_id: 1, node: 3, reveal: { input: "near" }, salt: "aa" })),
    '{"step_hidden":{"flow_id":1,"node":3,"reveal":{"input":"near"},"salt":"aa"}}', "exactly the bytes the brain parses");
});

test("a request with no args still names its method", () => {
  assert.deepEqual(wrap("unstick"), { unstick: {} });
});

test("only vault method names can be wrapped", () => {
  for (const bad of ["", "Step", "step-hidden", "x}", '"a":1']) assert.throws(() => wrap(bad, {}), /not a vault method/);
});
