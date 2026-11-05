// Wallet sign-in with NEP-413 (signMessage), the standard NEAR wallets use to
// prove you control an account without sending a transaction.
//
// 1. The server hands out a one-time random nonce (a "challenge").
// 2. The wallet signs { message, nonce, recipient } with the account's key.
// 3. The server checks the signature, that the nonce is fresh and unused,
//    that the recipient is THIS server (so a signature made for another app
//    can't be replayed here), and that the key is a FULL-ACCESS key of the
//    account on chain (a leaked limited app key must not be enough).
// Then it issues a session token for the chat connection.

import { createHash, randomBytes } from "node:crypto";
import { serialize } from "borsh";
import { PublicKey } from "near-api-js";

export const LOGIN_MESSAGE = "Sign in to Bazaar chat";
const NEP413_TAG = 2 ** 31 + 413;
const CHALLENGE_TTL_MS = 5 * 60_000;
const SESSION_TTL_MS = 24 * 3_600_000;
/** Outstanding challenges kept at most; beyond it the oldest are dropped (flood guard). */
const MAX_CHALLENGES = 10_000;

const PAYLOAD_SCHEMA = {
  struct: {
    message: "string",
    nonce: { array: { type: "u8", len: 32 } },
    recipient: "string",
    callbackUrl: { option: "string" },
  },
} as const;

/** The 32-byte hash a NEP-413 wallet signs. */
export function nep413Hash(message: string, nonce: Uint8Array, recipient: string, callbackUrl?: string): Uint8Array {
  const tag = new Uint8Array(4);
  new DataView(tag.buffer).setUint32(0, NEP413_TAG, true);
  const payload = serialize(PAYLOAD_SCHEMA, { message, nonce: Array.from(nonce), recipient, callbackUrl: callbackUrl ?? null });
  return createHash("sha256").update(tag).update(payload).digest();
}

export interface LoginRequest {
  accountId: string;
  publicKey: string; // "ed25519:..."
  signature: string; // base64, as wallets return it
  nonce: string; // base64, the challenge from /challenge
}

/** On-chain checks the server needs (real RPC in production, a fake in tests). */
export interface KeyChecker {
  isFullAccessKey(accountId: string, publicKey: string): Promise<boolean>;
}

export class Auth {
  private challenges = new Map<string, number>(); // nonce (base64) -> expiry
  private sessions = new Map<string, { accountId: string; expires: number }>();

  constructor(private recipient: string, private keys: KeyChecker, private now: () => number = Date.now) {}

  challenge(): { nonce: string; message: string; recipient: string } {
    this.sweep();
    const nonce = randomBytes(32).toString("base64");
    if (this.challenges.size >= MAX_CHALLENGES) {
      this.challenges.delete(this.challenges.keys().next().value!); // oldest first (Map keeps insertion order)
    }
    this.challenges.set(nonce, this.now() + CHALLENGE_TTL_MS);
    return { nonce, message: LOGIN_MESSAGE, recipient: this.recipient };
  }

  /** Returns a session token, or throws with the reason. */
  async login(req: LoginRequest): Promise<string> {
    const expiry = this.challenges.get(req.nonce);
    if (expiry === undefined || expiry < this.now()) throw new Error("unknown or expired challenge");
    this.challenges.delete(req.nonce); // single use, even if the rest fails
    if (!/^[a-z0-9._-]{2,64}$/.test(req.accountId)) throw new Error("bad account id");
    const nonce = Buffer.from(req.nonce, "base64");
    if (nonce.length !== 32) throw new Error("bad nonce");
    let ok = false;
    try {
      const hash = nep413Hash(LOGIN_MESSAGE, nonce, this.recipient);
      ok = PublicKey.fromString(req.publicKey).verify(hash, Buffer.from(req.signature, "base64"));
    } catch {
      ok = false;
    }
    if (!ok) throw new Error("bad signature");
    if (!(await this.keys.isFullAccessKey(req.accountId, req.publicKey))) {
      throw new Error("that key is not a full-access key of this account");
    }
    const token = randomBytes(32).toString("base64url");
    this.sessions.set(token, { accountId: req.accountId, expires: this.now() + SESSION_TTL_MS });
    return token;
  }

  /** The account behind a session token, or null. */
  whoIs(token: string | null | undefined): string | null {
    if (!token) return null;
    const s = this.sessions.get(token);
    if (!s || s.expires < this.now()) return null;
    return s.accountId;
  }

  private sweep() {
    const t = this.now();
    for (const [k, v] of this.challenges) if (v < t) this.challenges.delete(k);
    for (const [k, v] of this.sessions) if (v.expires < t) this.sessions.delete(k);
  }
}
