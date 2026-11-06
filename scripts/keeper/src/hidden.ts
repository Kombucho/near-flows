// Hidden flows, off chain: the owner's plan and the salts that unlock it.
//
// On chain a hidden flow is only input tokens and fingerprints,
// sha256(salt || borsh(node)) per node (contracts/vault/src/hidden.rs).
// The plain nodes and salts live here, in ~/.flows/hidden/<vault>.json,
// readable only by the owner and the keeper they share it with.
//
// borsh is written out by hand so the byte layout is visible and matches the
// vault's types field by field; a test pins it to a fingerprint the vault computed.

import { createHash, randomBytes } from "node:crypto";
import { readFileSync, writeFileSync, mkdirSync, existsSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

type Deposit = { kind: "none" } | { kind: "one_yocto" } | { kind: "input" } | { kind: "fixed"; yocto: string };
type Call =
  | { call: "ft"; receiver: string; msg: string; tgas: number }
  | { call: "fn"; contract: string; method: string; args: string; deposit: Deposit; tgas: number };
type Trigger =
  | { when: "now" }
  | { when: "after"; at_ns: number }
  | { when: "every"; every_ns: number; chunk: string }
  | { when: "price_below"; price: string }
  | { when: "price_above"; price: string }
  | { when: "manual" };
// A node on another chain (contracts/vault/src/remote.rs).
type Remote = {
  call:
    | { chain: "evm"; template: string; amount_leaf?: number | null; min_leaf?: number | null; deadline_leaf?: number | null }
    | { chain: "sol"; template: string; amount_field?: number | null; min_field?: number | null };
  out?: string | null;
  max_out_per_in?: string | null;
  max_out?: string | null;
  price?: { feed: string; quote?: string | null } | null;
};
export type Node = {
  input: string;
  action: {
    label: string;
    calls: Call[];
    out: string | null;
    min_out_per_in: string | null;
    price: { contract: string; method: string; args: string; per: string } | null;
    scale_per_in?: string | null;
    market?: { sell: string; buy: string; dec_in: number; dec_out: number; slippage_bps: number } | null;
    remote?: Remote | null;
  };
  when: Trigger;
  next?: { to: number; bps: number }[];
};

class W {
  parts: Buffer[] = [];
  u8(v: number) { this.parts.push(Buffer.from([v])); }
  u16(v: number) { const b = Buffer.alloc(2); b.writeUInt16LE(v); this.parts.push(b); }
  u32(v: number) { const b = Buffer.alloc(4); b.writeUInt32LE(v); this.parts.push(b); }
  u64(v: number | bigint) { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(v)); this.parts.push(b); }
  u128(v: string | bigint) {
    const x = BigInt(v);
    const b = Buffer.alloc(16);
    b.writeBigUInt64LE(x & 0xffffffffffffffffn, 0);
    b.writeBigUInt64LE(x >> 64n, 8);
    this.parts.push(b);
  }
  str(s: string) { const b = Buffer.from(s, "utf8"); this.u32(b.length); this.parts.push(b); }
  opt<T>(v: T | null | undefined, f: (v: T) => void) { if (v === null || v === undefined) this.u8(0); else { this.u8(1); f(v); } }
  bytes() { return Buffer.concat(this.parts); }
}

/** borsh(Node<Recipe>), in the vault's field order. */
export function borshNode(n: Node): Buffer {
  const w = new W();
  w.str(n.input);
  const a = n.action;
  w.str(a.label);
  w.u32(a.calls.length);
  for (const c of a.calls) {
    if (c.call === "ft") { w.u8(0); w.str(c.receiver); w.str(c.msg); w.u64(c.tgas); }
    else {
      w.u8(1); w.str(c.contract); w.str(c.method); w.str(c.args);
      const d = c.deposit;
      if (d.kind === "none") w.u8(0);
      else if (d.kind === "one_yocto") w.u8(1);
      else if (d.kind === "input") w.u8(2);
      else { w.u8(3); w.u128(d.yocto); }
      w.u64(c.tgas);
    }
  }
  w.opt(a.out, (v) => w.str(v));
  w.opt(a.min_out_per_in, (v) => w.u128(v));
  w.opt(a.price, (p) => { w.str(p.contract); w.str(p.method); w.str(p.args); w.u128(p.per); });
  w.opt(a.scale_per_in, (v) => w.u128(v));
  w.opt(a.market, (m) => { w.str(m.sell); w.str(m.buy); w.u8(m.dec_in); w.u8(m.dec_out); w.u32(m.slippage_bps); });
  w.opt(a.remote, (r) => {
    const c = r.call;
    const u8 = (v: number) => w.u8(v);
    if (c.chain === "evm") { w.u8(0); w.str(c.template); w.opt(c.amount_leaf, u8); w.opt(c.min_leaf, u8); w.opt(c.deadline_leaf, u8); }
    else { w.u8(1); w.str(c.template); w.opt(c.amount_field, u8); w.opt(c.min_field, u8); }
    w.opt(r.out, (v) => w.str(v));
    w.opt(r.max_out_per_in, (v) => w.u128(v));
    w.opt(r.max_out, (v) => w.u128(v));
    w.opt(r.price, (p) => { w.str(p.feed); w.opt(p.quote, (q) => w.str(q)); });
  });
  const t = n.when;
  switch (t.when) {
    case "now": w.u8(0); break;
    case "after": w.u8(1); w.u64(t.at_ns); break;
    case "every": w.u8(2); w.u64(t.every_ns); w.u128(t.chunk); break;
    case "price_below": w.u8(3); w.u128(t.price); break;
    case "price_above": w.u8(4); w.u128(t.price); break;
    case "manual": w.u8(5); break;
  }
  const next = n.next ?? [];
  w.u32(next.length);
  for (const r of next) { w.u16(r.to); w.u16(r.bps); }
  return w.bytes();
}

/** The node's on-chain fingerprint: hex(sha256(salt || borsh(node))). */
export function fingerprint(n: Node, saltHex: string): string {
  const salt = Buffer.from(saltHex, "hex");
  if (salt.length !== 32) throw new Error("salt: 32 bytes, hex");
  return createHash("sha256").update(salt).update(borshNode(n)).digest("hex");
}

/** A fresh salt per node, every time: a reused salt would let one revealed node unlock guesses at another. */
export const newSalt = () => randomBytes(32).toString("hex");

// ---------- the owner's store ----------

export type HiddenFlow = { nodes: Node[]; salts: string[] };
type Store = Record<string, HiddenFlow>; // flow id -> plan

const dir = () => join(homedir(), ".flows", "hidden");
const file = (vault: string) => join(dir(), `${vault}.json`);

export function loadStore(vault: string): Store {
  return existsSync(file(vault)) ? JSON.parse(readFileSync(file(vault), "utf8")) : {};
}

export function saveStore(vault: string, s: Store) {
  mkdirSync(dir(), { recursive: true, mode: 0o700 });
  writeFileSync(file(vault), JSON.stringify(s, null, 2), { mode: 0o600 });
}

/** The plain nodes of a hidden flow, only if every one matches the chain's fingerprints. */
export function unlock(plan: HiddenFlow | undefined, prints: string[]): Node[] | null {
  if (!plan || plan.nodes.length !== prints.length) return null;
  return plan.nodes.every((n, i) => fingerprint(n, plan.salts[i]) === prints[i]) ? plan.nodes : null;
}
