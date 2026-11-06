// Stealth vaults, off chain: the owner signs commands with a key, the keeper relays them.
//
// A stealth vault is owned by an Ed25519 key, not an account. The owner signs a
// command here, any account sends it to the vault's `exec` door and pays the gas,
// and the brain checks the signature (contracts/brain/src/stealth.rs). The owner's
// wallet never touches the vault. The relay vault's `exec` takes the same args the
// full vault did, so only the gas changed (see execTgas).
//
// The key lives in a keypair file in near-cli's format
// ({"public_key": "ed25519:...", "private_key": "ed25519:..."}). Keep it apart from
// your wallet: a key that ever signed for your wallet links you.
//
// The signed bytes are spelled out in `message` and pinned by stealth.test.ts to the
// same vector the brain's own test pins (contracts/brain/src/stealth.rs).

import { createPrivateKey, createPublicKey, sign as edSign, randomBytes } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { Account, baseDecode, baseEncode } from "near-api-js";

export type Command = { method: string; args: Record<string, unknown> | string; tgas?: number };
export type Signed = { method: string; args: string; nonce: string; expires_ns: string; tgas: number; signature: string };

/** What the relay gives the brain's first round (relay GAS_HANDLE). */
export const HANDLE_TGAS = 100;
/** The signed gas for a `step`: the brain reads the trigger price, the market floor and the
 *  balances in this round; the recipe's own rounds come out of what's left (gas_mainnet.rs:
 *  a 50 Tgas swap with a market floor fits in 300 with 80 signed; 60 ran out). */
export const STEP_TGAS = 80;

/** Gas to attach to `exec`: the brain refuses unless its round has the signed gas + 10 left
 *  after HANDLE_TGAS; 5 more covers the relay's own work. Never over mainnet's 300 cap. */
export function execTgas(tgas: number): number {
  const need = tgas + 10 + HANDLE_TGAS + 5;
  if (need > 300) throw new Error(`a command signed for ${tgas} Tgas needs ${need} Tgas: mainnet allows 300, sign for at most ${300 - 115}`);
  return need;
}

/** The exact text the owner signs. Must match stealth::message in the vault, byte for byte. */
export function message(vault: string, nonce: bigint, expiresNs: bigint, tgas: number, method: string, args: string): string {
  return `near-flows stealth v1\nvault: ${vault}\nnonce: ${nonce}\nexpires_ns: ${expiresNs}\ntgas: ${tgas}\nmethod: ${method}\nargs: ${args}`;
}

/** An owner key from its 32-byte seed. */
export class OwnerKey {
  private readonly key;
  readonly publicKey: string; // ed25519:<base58>

  constructor(readonly seed: Uint8Array) {
    if (seed.length !== 32) throw new Error("an ed25519 seed is 32 bytes");
    // PKCS#8 wrapping of a raw Ed25519 seed (RFC 8410).
    const der = Buffer.concat([Buffer.from("302e020100300506032b657004220420", "hex"), Buffer.from(seed)]);
    this.key = createPrivateKey({ key: der, format: "der", type: "pkcs8" });
    const spki = createPublicKey(this.key).export({ format: "der", type: "spki" });
    this.publicKey = "ed25519:" + baseEncode(spki.subarray(spki.length - 32));
  }

  static random(): OwnerKey {
    return new OwnerKey(randomBytes(32));
  }

  /** Read a near-cli keypair file (private_key = ed25519:<base58 of seed || public key>). */
  static load(file: string): OwnerKey {
    const sk: string = JSON.parse(readFileSync(file, "utf8")).private_key;
    if (!sk.startsWith("ed25519:")) throw new Error("the owner key must be ed25519");
    return new OwnerKey(baseDecode(sk.slice(8)).subarray(0, 32));
  }

  save(file: string) {
    const pub = baseDecode(this.publicKey.slice(8));
    const sk = "ed25519:" + baseEncode(Buffer.concat([Buffer.from(this.seed), Buffer.from(pub)]));
    writeFileSync(file, JSON.stringify({ public_key: this.publicKey, private_key: sk }, null, 2) + "\n", { mode: 0o600 });
  }

  /** Sign `cmd` for `vault`. Nonce: anything above the vault's last one (get_nonce); the time in ns works. */
  sign(vault: string, cmd: Command, nonce: bigint, expiresNs: bigint): Signed {
    const args = typeof cmd.args === "string" ? cmd.args : JSON.stringify(cmd.args);
    const tgas = cmd.tgas ?? 50;
    const msg = message(vault, nonce, expiresNs, tgas, cmd.method, args);
    const signature = edSign(null, Buffer.from(msg, "utf8"), this.key).toString("hex");
    return { method: cmd.method, args, nonce: nonce.toString(), expires_ns: expiresNs.toString(), tgas, signature };
  }
}

/** Send a signed command from `relayer` (it pays the gas). Returns what the command returned. */
export async function relay<T>(relayer: Account, vault: string, s: Signed): Promise<T> {
  execTgas(s.tgas); // refuse up front what can never fit
  // Always the full 300: a signed `step` runs its recipe's rounds on what's left after
  // the signed gas, and unused gas is refunded to the relayer anyway.
  return relayer.callFunction<any>({ contractId: vault, methodName: "exec", args: s, gas: 300n * 10n ** 12n }) as Promise<T>;
}
