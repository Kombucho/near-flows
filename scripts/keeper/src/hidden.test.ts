import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fingerprint, newSalt, unlock, type Node } from "./hidden.ts";

const node: Node = JSON.parse(readFileSync(new URL("../../../contracts/vault/tests-data/fingerprint-node.json", import.meta.url), "utf8"));
const salt = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

test("the keeper's fingerprint matches the vault's, byte for byte", () => {
  // Same value pinned in contracts/vault/src/tests.rs, computed there by the vault's own code.
  assert.equal(fingerprint(node, salt), "ad13adedfaed79107498ceb3bb79901e8dff115529609033291981e71bd0d36b");
  // A remote node (an EVM ladder rung), every remote field set.
  const remote: Node = JSON.parse(readFileSync(new URL("../../../contracts/vault/tests-data/fingerprint-remote-node.json", import.meta.url), "utf8"));
  assert.equal(fingerprint(remote, salt), "cfb851d722d274c36161359966c9a849ee9b7a5a947156f6acd0f7aa2acfe35c");
});

test("any change to a node, or another salt, changes its fingerprint", () => {
  const greedy = structuredClone(node);
  greedy.action.min_out_per_in = "1";
  assert.notEqual(fingerprint(greedy, salt), fingerprint(node, salt));
  assert.notEqual(fingerprint(node, newSalt()), fingerprint(node, salt));
});

test("a stored plan unlocks only if every node matches the chain", () => {
  const prints = [fingerprint(node, salt)];
  assert.deepEqual(unlock({ nodes: [node], salts: [salt] }, prints), [node]);
  assert.equal(unlock({ nodes: [node], salts: [newSalt()] }, prints), null);
  assert.equal(unlock(undefined, prints), null);
});
