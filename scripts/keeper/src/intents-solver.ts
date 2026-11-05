// Testnet solver for a vault's own intents (there is no Solver Bus on testnet).
//
//   npm run intents -- --vault <vault> --in <token> --out <token> --amount <base units>
//
// 1. Asks the vault to sign "give `amount` of IN, get at least its floor of OUT"
//    (the vault fetches the verifier's salt and gets an MPC Ed25519 signature).
// 2. Signs the matching opposite intent with the solver's own key, paying exactly
//    the vault's minimum (fee grossed up), and submits both to execute_intents.
// The verifier checks both signatures and that every token balances. The solver is
// only a counterparty: if it offers less than the vault's minimum, nothing settles.
//
// The solver key lives in SOLVER_KEY_FILE (default ~/.flows/solver-<keeper>.json), never in the repo.
// Environment: KEEPER_ACCOUNT_ID (the solver account), NEAR_RPC, KEEPER_PRIVATE_KEY or near-cli key file.

import { createPrivateKey, createPublicKey, generateKeyPairSync, randomBytes, sign as edSign } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { parseArgs } from "node:util";
import { Chain, loadKey } from "./chain.ts";
import { afterFee, gross, nep413Hash, swapMessage, versionedNonce } from "./intents.ts";

const { values: o } = parseArgs({ options: { vault: { type: "string" }, in: { type: "string" }, out: { type: "string" }, amount: { type: "string" } } });
if (!o.vault || !o.in || !o.out || !o.amount) throw new Error("--vault --in --out --amount are required");
const network = process.env.NEAR_NETWORK ?? "testnet";
const solver = process.env.KEEPER_ACCOUNT_ID ?? "";
const chain = new Chain(process.env.NEAR_RPC ?? "https://test.rpc.fastnear.com", o.vault, solver, loadKey(network, solver));
const TGAS = 10n ** 12n;
const B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const b58 = (b: Buffer) => { let n = BigInt("0x" + (b.toString("hex") || "0")), s = ""; while (n > 0n) { s = B58[Number(n % 58n)] + s; n /= 58n; } for (const x of b) { if (x) break; s = "1" + s; } return s; };
const logsOf = (r: any): string[] => (r?.receipts_outcome ?? []).flatMap((x: any) => x.outcome.logs);

const cfg = await chain.view<{ verifier: string }>(o.vault, "get_intents");
const verifier = cfg.verifier;

// The solver's own Ed25519 key on Intents (made once, registered once).
const keyFile = process.env.SOLVER_KEY_FILE ?? join(homedir(), ".flows", `solver-${solver}.json`);
if (!existsSync(keyFile)) {
  const { privateKey } = generateKeyPairSync("ed25519");
  mkdirSync(dirname(keyFile), { recursive: true });
  writeFileSync(keyFile, JSON.stringify({ pkcs8: privateKey.export({ format: "der", type: "pkcs8" }).toString("base64") }), { mode: 0o600 });
}
const priv = createPrivateKey({ key: Buffer.from(JSON.parse(readFileSync(keyFile, "utf8")).pkcs8, "base64"), format: "der", type: "pkcs8" });
const pub = createPublicKey(priv).export({ format: "der", type: "spki" }).subarray(-32);
const pubStr = `ed25519:${b58(pub)}`;
const known = await chain.view<string[]>(verifier, "public_keys_of", { account_id: solver });
if (!known.includes(pubStr)) {
  await chain.call(verifier, "add_public_key", { public_key: pubStr }, 30n * TGAS, 1n);
  console.log(`registered the solver key ${pubStr}`);
}

// 1. The vault signs its own intent.
const r1: any = await chain.callRaw(o.vault, "intents_sign_swap", { token_in: o.in, token_out: o.out, amount_in: o.amount, min_out: "0", valid_secs: 600 }, 300n * TGAS);
const ev = logsOf(r1).find((l) => l.includes('"intents_signed"'));
if (!ev) throw new Error(`the vault didn't sign: ${logsOf(r1).join(" | ").slice(0, 300)}`);
const vaultSigned = JSON.parse(ev.slice("EVENT_JSON:".length)).data[0].signed;
const diff: Record<string, string> = JSON.parse(vaultSigned.payload.message).intents[0].diff;
console.log("vault's intent (MPC-signed):", JSON.stringify(diff));

// 2. The matching side: give exactly the vault's minimum (plus our fee), take what it gives (minus its fee).
const fee = BigInt(await chain.view<number>(verifier, "fee"));
const vaultGives = -BigInt(diff[`nep141:${o.in}`]);
const vaultWants = BigInt(diff[`nep141:${o.out}`]);
const salt = Buffer.from(await chain.view<string>(verifier, "current_salt"), "hex");
const deadline = BigInt(Date.now() + 600_000) * 1_000_000n;
const msg = swapMessage(solver, deadline, [o.out, gross(vaultWants, fee)], [o.in, afterFee(vaultGives, fee)]);
const nonce = versionedNonce(salt, deadline, randomBytes(15));
const sig = edSign(null, nep413Hash(msg, nonce, verifier), priv);
const solverSigned = { standard: "nep413", payload: { message: msg, nonce: nonce.toString("base64"), recipient: verifier }, public_key: pubStr, signature: `ed25519:${b58(sig)}` };
console.log("solver's intent:", JSON.stringify(JSON.parse(msg).intents[0].diff));

// 3. Both to the verifier.
const r2: any = await chain.callRaw(verifier, "execute_intents", { signed: [vaultSigned, solverSigned] }, 300n * TGAS);
const failed = (r2?.receipts_outcome ?? []).find((x: any) => x.outcome.status?.Failure);
console.log(failed ? `NOT settled: ${JSON.stringify(failed.outcome.status.Failure).slice(0, 300)}` : `settled: tx ${r2?.transaction?.hash}`);
