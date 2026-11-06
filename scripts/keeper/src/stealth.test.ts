import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createPublicKey, verify } from "node:crypto";
import { baseDecode } from "near-api-js";
import { execTgas, message, OwnerKey, STEP_TGAS } from "./stealth.ts";

// The same vector is pinned in contracts/vault/src/stealth.rs, where the vault's own
// code builds the message and checks this signature with env::ed25519_verify.
const seed = Uint8Array.from({ length: 32 }, (_, i) => i);
const vault = "k7xq2m.vaults.near";
const cmd = { method: "withdraw_to", args: { token: "near", amount: "1000", to: "fresh-7f3a.near", via_intents: false }, tgas: 50 };
const nonce = 1762400000000000000n;
const expires = 1762400600000000000n;
const PIN_TEXT =
  'near-flows stealth v1\nvault: k7xq2m.vaults.near\nnonce: 1762400000000000000\nexpires_ns: 1762400600000000000\ntgas: 50\nmethod: withdraw_to\nargs: {"token":"near","amount":"1000","to":"fresh-7f3a.near","via_intents":false}';
const PIN_KEY = "ed25519:FAe4sisG95oZ42w7buUn5qEE4TAnfTTFPiguZUHmhiF";
const PIN_SIG = "342a99949d4e2c41a51f4b5e8dc5b85519f0215d916ef5b9387e68392197c8f487903f25305dbe161e6ae895fd70276a4340bd3d73b0cee57db67d7b4e0ea601";

test("the signed text matches the vault's, byte for byte", () => {
  assert.equal(message(vault, nonce, expires, 50, cmd.method, JSON.stringify(cmd.args)), PIN_TEXT);
});

test("the owner's signature matches the one the vault accepts", () => {
  const k = new OwnerKey(seed);
  const s = k.sign(vault, cmd, nonce, expires);
  assert.equal(k.publicKey, PIN_KEY);
  assert.equal(s.signature, PIN_SIG);
  assert.deepEqual([s.nonce, s.expires_ns, s.tgas, s.method], ["1762400000000000000", "1762400600000000000", 50, "withdraw_to"], "u64s travel as strings");
});

test("a signature verifies only for the vault and command it was made for", () => {
  const k = OwnerKey.random();
  const s = k.sign(vault, cmd, nonce, expires);
  const pub = createPublicKey({ key: Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), Buffer.from(baseDecode(k.publicKey.slice(8)))]), format: "der", type: "spki" });
  const ok = (text: string) => verify(null, Buffer.from(text), pub, Buffer.from(s.signature, "hex"));
  assert.ok(ok(message(vault, nonce, expires, 50, s.method, s.args)));
  assert.ok(!ok(message("other.vaults.near", nonce, expires, 50, s.method, s.args)));
  assert.ok(!ok(message(vault, nonce + 1n, expires, 50, s.method, s.args)));
  assert.ok(!ok(message(vault, nonce, expires, 50, s.method, s.args.replace("fresh-7f3a", "mallory"))));
});

test("a keypair file round-trips in near-cli's format", () => {
  const k = OwnerKey.random();
  const file = join(mkdtempSync(join(tmpdir(), "stealth-")), "owner.json");
  k.save(file);
  const back = OwnerKey.load(file);
  assert.equal(back.publicKey, k.publicKey);
  assert.deepEqual(Buffer.from(back.seed), Buffer.from(k.seed));
});

test("exec needs the signed gas, the brain's 10 and the relay's 100 first round, never over 300", () => {
  assert.equal(execTgas(50), 165);
  assert.equal(execTgas(STEP_TGAS), 195);
  assert.equal(execTgas(185), 300);
  assert.throws(() => execTgas(200), /mainnet allows 300/, "a step signed for 200 can never run on mainnet");
});
