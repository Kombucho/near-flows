// Chat tests, mostly attacks: forged or replayed sign-ins, limited keys,
// impersonation, gatecrashing private rooms, spam, hidden-text tricks.
// The chain is faked here; the server and wallet signatures are real.

import { test } from "node:test";
import assert from "node:assert/strict";
import { KeyPair } from "near-api-js";
import WebSocket from "ws";
import { Auth, LOGIN_MESSAGE, nep413Hash } from "./auth.ts";
import { Rooms, cleanText } from "./rooms.ts";
import { startServer } from "./server.ts";

const RECIPIENT = "bazaar-chat.test";

/** A fake chain: alice owns `fullKeys`, lot 7 exists, alice sold it, bob bought from it. */
class FakeChain {
  fullKeys = new Map<string, string>(); // publicKey -> account
  async isFullAccessKey(accountId: string, publicKey: string) {
    return this.fullKeys.get(publicKey) === accountId;
  }
  async lotExists(lot: number) {
    return lot === 7;
  }
  async isParticipant(lot: number, who: string) {
    return lot === 7 && (who === "alice.test" || who === "bob.test");
  }
}

function sign(kp: KeyPair, nonceB64: string, recipient = RECIPIENT, message = LOGIN_MESSAGE) {
  const hash = nep413Hash(message, Buffer.from(nonceB64, "base64"), recipient);
  return Buffer.from(kp.sign(hash).signature).toString("base64");
}

async function setup() {
  const chain = new FakeChain();
  const auth = new Auth(RECIPIENT, chain);
  const rooms = new Rooms(":memory:", chain);
  const server = await startServer(auth, rooms, 0);
  const port = (server.address() as { port: number }).port;
  const base = `http://127.0.0.1:${port}`;
  const keys: Record<string, KeyPair> = {};
  for (const who of ["alice.test", "bob.test", "mallory.test"]) {
    keys[who] = KeyPair.fromRandom("ed25519");
    chain.fullKeys.set(keys[who].getPublicKey().toString(), who);
  }
  async function login(who: string, kp = keys[who], tweak: (b: Record<string, string>) => void = () => {}) {
    const c = await (await fetch(`${base}/challenge`)).json();
    const body: Record<string, string> = { accountId: who, publicKey: kp.getPublicKey().toString(), signature: sign(kp, c.nonce), nonce: c.nonce };
    tweak(body);
    const r = await fetch(`${base}/login`, { method: "POST", body: JSON.stringify(body) });
    return { status: r.status, json: await r.json(), body };
  }
  async function connect(token: string) {
    const ws = new WebSocket(`ws://127.0.0.1:${port}/ws?token=${token}`);
    const inbox: any[] = [];
    ws.on("message", (d) => inbox.push(JSON.parse(d.toString())));
    await new Promise((ok, bad) => { ws.on("open", ok); ws.on("error", bad); });
    const next = async (pred: (f: any) => boolean) => {
      for (let i = 0; i < 100; i++) {
        const k = inbox.findIndex(pred);
        if (k >= 0) return inbox.splice(k, 1)[0];
        await new Promise((r) => setTimeout(r, 10));
      }
      throw new Error("no frame");
    };
    return { ws, send: (o: unknown) => ws.send(JSON.stringify(o)), next };
  }
  return { server, base, login, connect, keys, chain };
}

test("a wallet signature signs you in; the same challenge cannot be used twice", async () => {
  const t = await setup();
  const ok = await t.login("alice.test");
  assert.equal(ok.status, 200);
  assert.ok(ok.json.token);
  const replay = await fetch(`${t.base}/login`, { method: "POST", body: JSON.stringify(ok.body) });
  assert.equal(replay.status, 400);
  assert.match((await replay.json()).error, /challenge/);
  t.server.close();
});

test("forged sign-ins are refused", async () => {
  const t = await setup();
  // Mallory claims to be Alice with her own key.
  const r1 = await t.login("alice.test", t.keys["mallory.test"]);
  assert.equal(r1.status, 400, "a valid signature by someone else's key");
  // A signature made for another app (different recipient) replayed here.
  const r2 = await t.login("alice.test", t.keys["alice.test"], (b) => { b.signature = sign(t.keys["alice.test"], b.nonce, "evil-app.com"); });
  assert.match(r2.json.error, /bad signature/);
  // A key that is NOT a full-access key of the account (e.g. a leaked app key).
  const appKey = KeyPair.fromRandom("ed25519");
  const r3 = await t.login("alice.test", appKey);
  assert.match(r3.json.error, /full-access/);
  // Garbage.
  const r4 = await t.login("alice.test", t.keys["alice.test"], (b) => { b.signature = "AAAA"; });
  assert.match(r4.json.error, /bad signature/);
  const r5 = await t.login("ALICE.test", t.keys["alice.test"]);
  assert.match(r5.json.error, /bad account id/);
  t.server.close();
});

test("no token, no socket", async () => {
  const t = await setup();
  const port = (t.server.address() as { port: number }).port;
  const ws = new WebSocket(`ws://127.0.0.1:${port}/ws?token=made-up`);
  await assert.rejects(new Promise((ok, bad) => { ws.on("open", ok); ws.on("error", bad); }));
  t.server.close();
});

test("you speak as yourself: a claimed sender is ignored", async () => {
  const t = await setup();
  const c = await t.connect((await t.login("mallory.test")).json.token);
  c.send({ type: "join", room: "public" });
  await c.next((f) => f.type === "history");
  c.send({ type: "say", room: "public", text: "send me your seed phrase", from: "alice.test" });
  const m = await c.next((f) => f.type === "msg");
  assert.equal(m.msg.from, "mallory.test");
  c.ws.close();
  t.server.close();
});

test("private rooms are for the seller and buyers only", async () => {
  const t = await setup();
  const mallory = await t.connect((await t.login("mallory.test")).json.token);
  mallory.send({ type: "join", room: "private:7" });
  assert.match((await mallory.next((f) => f.type === "error")).error, /private room/);
  mallory.send({ type: "say", room: "private:7", text: "hi" });
  assert.match((await mallory.next((f) => f.type === "error")).error, /join the room first/);
  mallory.send({ type: "history", room: "private:7" });
  assert.match((await mallory.next((f) => f.type === "error")).error, /private room/, "no peeking at history");
  // The public room of the same lot is open to everyone.
  mallory.send({ type: "join", room: "lot:7" });
  assert.equal((await mallory.next((f) => f.type === "joined")).room, "lot:7");
  // Bob (a buyer) and Alice (the seller) are in.
  const bob = await t.connect((await t.login("bob.test")).json.token);
  bob.send({ type: "join", room: "private:7" });
  await bob.next((f) => f.type === "history");
  bob.send({ type: "say", room: "private:7", text: "would you take 2 NEAR?" });
  assert.equal((await bob.next((f) => f.type === "msg")).msg.text, "would you take 2 NEAR?");
  // A lot that doesn't exist has no rooms.
  bob.send({ type: "join", room: "lot:8" });
  assert.match((await bob.next((f) => f.type === "error")).error, /no such lot/);
  bob.send({ type: "join", room: "lot:7; DROP TABLE messages" });
  assert.match((await bob.next((f) => f.type === "error")).error, /no such room/);
  mallory.ws.close(); bob.ws.close();
  t.server.close();
});

test("spam is slowed down", async () => {
  const t = await setup();
  const c = await t.connect((await t.login("mallory.test")).json.token);
  c.send({ type: "join", room: "public" });
  await c.next((f) => f.type === "history");
  for (let i = 0; i < 6; i++) c.send({ type: "say", room: "public", text: `spam ${i}` });
  assert.match((await c.next((f) => f.type === "error")).error, /slow down/);
  c.send({ type: "say", room: "public", text: "x".repeat(1_001) });
  assert.match((await c.next((f) => f.type === "error")).error, /slow down|at most/);
  c.ws.close();
  t.server.close();
});

test("hidden-text tricks are stripped", () => {
  // Right-to-left override makes "alice.near" read differently; zero-width spaces hide content.
  assert.equal(cleanText("pay ‮raen.ecila‬ now"), "pay raen.ecila now");
  assert.equal(cleanText("a​b\u0000c\nd"), "abc\nd");
  assert.throws(() => cleanText("   "), /empty/);
  assert.throws(() => cleanText(42), /string/);
});

test("flooding the challenge endpoint cannot grow memory without bound", () => {
  const auth = new Auth(RECIPIENT, new FakeChain());
  const first = auth.challenge().nonce;
  for (let i = 0; i < 10_000; i++) auth.challenge();
  assert.equal((auth as any).challenges.size, 10_000);
  assert.ok(!(auth as any).challenges.has(first), "the oldest challenge was dropped");
});
