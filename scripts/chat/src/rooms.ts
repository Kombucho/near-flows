// Rooms, membership, message rules and storage. No networking in here, so it
// can be tested directly.
//
//   "public"       everyone signed in
//   "lot:<id>"     everyone signed in: the lot's public discussion (its lore, its community)
//   "private:<id>" only the lot's seller and the accounts that bought from it,
//                  as recorded ON CHAIN (bazaar view `is_participant`)

import { DatabaseSync } from "node:sqlite";

export const MAX_LEN = 1_000;
const RATE_WINDOW_MS = 10_000;
const RATE_MAX = 5; // messages per window per account
const MEMBER_CACHE_MS = 30_000;

export interface Membership {
  /** Does lot `lotId` exist on chain? */
  lotExists(lotId: number): Promise<boolean>;
  /** Seller or buyer of the lot (on chain). */
  isParticipant(lotId: number, accountId: string): Promise<boolean>;
}

export interface Message {
  id: number;
  room: string;
  from: string;
  text: string;
  ts: number;
}

type Room = { kind: "public" } | { kind: "lot"; lot: number } | { kind: "private"; lot: number };

/** Parse a room name; null if it isn't one. */
export function parseRoom(name: unknown): Room | null {
  if (typeof name !== "string") return null;
  if (name === "public") return { kind: "public" };
  const m = /^(lot|private):(\d{1,15})$/.exec(name);
  if (!m) return null;
  return { kind: m[1] as "lot" | "private", lot: Number(m[2]) };
}

/** Clean a message: trim, refuse empty or too long, drop control characters except newlines. */
export function cleanText(text: unknown): string {
  if (typeof text !== "string") throw new Error("text must be a string");
  const t = text.replace(/[\u0000-\u0009\u000B-\u001F\u007F​-‏‪-‮⁦-⁩]/g, "").trim();
  if (!t) throw new Error("empty message");
  if (t.length > MAX_LEN) throw new Error(`at most ${MAX_LEN} characters`);
  return t;
}

export class Rooms {
  private db: DatabaseSync;
  private sent = new Map<string, number[]>(); // account -> recent send times
  private cache = new Map<string, { ok: boolean; at: number }>();

  constructor(dbPath: string, private chain: Membership, private now: () => number = Date.now) {
    this.db = new DatabaseSync(dbPath);
    this.db.exec(`CREATE TABLE IF NOT EXISTS messages (
      id INTEGER PRIMARY KEY AUTOINCREMENT, room TEXT NOT NULL, sender TEXT NOT NULL, text TEXT NOT NULL, ts INTEGER NOT NULL);
      CREATE INDEX IF NOT EXISTS by_room ON messages (room, id);`);
  }

  /** May `accountId` read and write `room`? Throws with the reason if not. */
  async assertCanEnter(room: string, accountId: string): Promise<void> {
    const r = parseRoom(room);
    if (!r) throw new Error("no such room");
    if (r.kind === "public") return;
    if (!(await this.cached(`lot|${r.lot}`, () => this.chain.lotExists(r.lot)))) throw new Error("no such lot");
    if (r.kind === "lot") return;
    if (!(await this.cached(`p|${r.lot}|${accountId}`, () => this.chain.isParticipant(r.lot, accountId)))) {
      throw new Error("private room: only the seller and buyers of this lot");
    }
  }

  /** Store a message from a signed-in account (the sender is the session's account, never the client's claim). */
  async say(room: string, accountId: string, text: unknown): Promise<Message> {
    await this.assertCanEnter(room, accountId);
    const clean = cleanText(text);
    this.rateLimit(accountId);
    const ts = this.now();
    const res = this.db.prepare("INSERT INTO messages (room, sender, text, ts) VALUES (?, ?, ?, ?)").run(room, accountId, clean, ts);
    return { id: Number(res.lastInsertRowid), room, from: accountId, text: clean, ts };
  }

  /** The last `limit` messages of a room (before id `before`), oldest first. */
  async history(room: string, accountId: string, before?: number, limit = 50): Promise<Message[]> {
    await this.assertCanEnter(room, accountId);
    const n = Math.max(1, Math.min(100, Math.floor(limit)));
    const b = Number.isSafeInteger(before) ? before! : Number.MAX_SAFE_INTEGER;
    const rows = this.db.prepare("SELECT id, room, sender, text, ts FROM messages WHERE room = ? AND id < ? ORDER BY id DESC LIMIT ?")
      .all(room, b, n) as Array<{ id: number; room: string; sender: string; text: string; ts: number }>;
    return rows.reverse().map((r) => ({ id: r.id, room: r.room, from: r.sender, text: r.text, ts: r.ts }));
  }

  private rateLimit(accountId: string) {
    const t = this.now();
    const recent = (this.sent.get(accountId) ?? []).filter((x) => x > t - RATE_WINDOW_MS);
    if (recent.length >= RATE_MAX) throw new Error("slow down");
    recent.push(t);
    this.sent.set(accountId, recent);
  }

  /** Cache on-chain answers briefly. A "yes" can't be revoked on chain; a "no" is retried after 30s. */
  private async cached(key: string, ask: () => Promise<boolean>): Promise<boolean> {
    const hit = this.cache.get(key);
    if (hit && (hit.ok || hit.at > this.now() - MEMBER_CACHE_MS)) return hit.ok;
    const ok = await ask();
    this.cache.set(key, { ok, at: this.now() });
    return ok;
  }
}
