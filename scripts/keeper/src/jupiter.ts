// Jupiter swap for a vault, signed through NEAR Chain Signatures.
//
//   npm run jupiter -- --vault <vault> --in <mint> --out <mint> --amount <base units>
//   ... --broadcast     send it (MAINNET Solana: off by default; the default only simulates)
//
// 1. Ask Jupiter for a route and a transaction for the vault's Solana address,
//    built so the vault can check everything: no lookup tables (legacy tx),
//    no SOL wrap/unwrap, shared accounts, a fixed priority price.
// 2. Hand the MESSAGE to the vault (`sol_sign_swap`). The vault checks every
//    byte against its owner's policy and asks the MPC signer; a refusal ends here.
// 3. Put the signature in the transaction and simulate it on Solana (signature
//    checked by the validator), or broadcast it with --broadcast.
// The helper never holds keys or funds: the worst it can do is not show up.
//
// Environment: KEEPER_ACCOUNT_ID, NEAR_RPC, KEEPER_PRIVATE_KEY (or near-cli key file),
// SOLANA_RPC (default mainnet-beta), JUPITER_API (default https://lite-api.jup.ag/swap/v1).

import { createPublicKey, verify } from "node:crypto";
import { parseArgs } from "node:util";
import { Chain, loadKey } from "./chain.ts";
import { messageOf, withSignature } from "./solana.ts";

const { values: o } = parseArgs({
  options: {
    vault: { type: "string" }, in: { type: "string" }, out: { type: "string" }, amount: { type: "string" },
    slippage: { type: "string", default: "50" }, "cu-price": { type: "string", default: "50000" },
    broadcast: { type: "boolean", default: false },
  },
});
if (!o.vault || !o.in || !o.out || !o.amount) throw new Error("--vault --in --out --amount are required");
const network = process.env.NEAR_NETWORK ?? "testnet";
const keeperId = process.env.KEEPER_ACCOUNT_ID ?? "";
const chain = new Chain(process.env.NEAR_RPC ?? "https://test.rpc.fastnear.com", o.vault, keeperId, loadKey(network, keeperId));
const jup = process.env.JUPITER_API ?? "https://lite-api.jup.ag/swap/v1";
const solRpc = process.env.SOLANA_RPC ?? "https://api.mainnet-beta.solana.com";
const b64 = (b: Uint8Array) => Buffer.from(b).toString("base64");

const sol = await chain.view<{ address: string | null }>(o.vault, "get_sol");
if (!sol?.address) throw new Error("the vault has no Solana address yet (owner: sol_setup)");
console.log(`vault Solana address ${sol.address}`);

const q = new URLSearchParams({ inputMint: o.in, outputMint: o.out, amount: o.amount, slippageBps: o.slippage!, asLegacyTransaction: "true", platformFeeBps: "0" });
const quote = await (await fetch(`${jup}/quote?${q}`)).json();
if (!quote.outAmount) throw new Error(`no quote: ${JSON.stringify(quote).slice(0, 200)}`);
console.log(`quote: ${o.amount} in -> ${quote.outAmount} out (min ${quote.otherAmountThreshold}), route ${quote.routePlan?.map((r: any) => r.swapInfo?.label).join(" > ")}`);
const swap = await (await fetch(`${jup}/swap`, {
  method: "POST", headers: { "content-type": "application/json" },
  body: JSON.stringify({ quoteResponse: quote, userPublicKey: sol.address, wrapAndUnwrapSol: false, useSharedAccounts: true,
    asLegacyTransaction: true, computeUnitPriceMicroLamports: Number(o["cu-price"]), dynamicComputeUnitLimit: false }),
})).json();
if (!swap.swapTransaction) throw new Error(`no transaction: ${JSON.stringify(swap).slice(0, 200)}`);
const message = messageOf(Buffer.from(swap.swapTransaction, "base64"));

const r: any = await chain.callRaw(o.vault, "sol_sign_swap", { message: b64(message) }, 300n * 10n ** 12n);
const logs: string[] = (r?.receipts_outcome ?? []).flatMap((x: any) => x.outcome.logs);
const signed = logs.find((l) => l.includes('"sol_signed"'));
if (!signed) throw new Error(`not signed: ${logs.join(" | ").slice(0, 400)}`);
const sigHex = /"signature":"([0-9a-f]{128})"/.exec(signed)![1];
const sig = Buffer.from(sigHex, "hex");
// Never pass on a signature we can't verify: Ed25519 over the exact message, by the vault's Solana key.
const B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const pub = Buffer.from([...sol.address].reduce((n, c) => n * 58n + BigInt(B58.indexOf(c)), 0n).toString(16).padStart(64, "0"), "hex");
const spki = createPublicKey({ key: Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), pub]), format: "der", type: "spki" });
if (!verify(null, message, spki, sig)) throw new Error("the signature does not verify: not broadcasting");
const tx = withSignature(message, sig);
console.log(`signed by the vault (${sigHex.slice(0, 16)}...), Ed25519 signature verified against ${sol.address}`);

const method = o.broadcast ? "sendTransaction" : "simulateTransaction";
const params = o.broadcast ? [b64(tx), { encoding: "base64" }] : [b64(tx), { encoding: "base64", sigVerify: true, replaceRecentBlockhash: false }];
const res = await (await fetch(solRpc, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }) })).json();
console.log(`${method}:`, JSON.stringify(res.result ?? res.error).slice(0, 600));
