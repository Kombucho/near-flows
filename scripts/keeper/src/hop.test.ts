import { test } from "node:test";
import assert from "node:assert/strict";
import { asset, bridgeChain, hopChainOf, hopReport, landsAs, quoteBody, rpcBody, tokenTerms, withdrawalState, withdrawArgs } from "./hop.ts";

test("only POA tokens hop, each to its own chain (same rule as the brain's hop.rs)", () => {
  assert.deepEqual(hopChainOf("sol.omft.near"), { kind: "sol" });
  assert.deepEqual(hopChainOf("sol-5ce3bf3a31af18be40ba30f721101b4341690186.omft.near"), { kind: "sol" });
  assert.deepEqual(hopChainOf("base-0x833589fcd6edb6e08f4c7c32d4f71b54bda02913.omft.near"), { kind: "evm", chainId: 8453 });
  assert.deepEqual(hopChainOf("hood.omft.near"), { kind: "evm", chainId: 4663 });
  for (const t of ["wrap.near", "btc.omft.near", "sol.omftx.near", "omft.near", "sol-0xaad7.omdep.near"]) assert.equal(hopChainOf(t), null, t);
  assert.equal(bridgeChain({ kind: "sol" }), "sol:mainnet");
  assert.equal(bridgeChain({ kind: "evm", chainId: 4663 }), "eth:4663");
  assert.equal(landsAs({ kind: "evm", chainId: 8453 }), "evm:8453:");
});

test("the withdrawal the brain builds: the token is its own receiver, the address in the memo", () => {
  assert.deepEqual(withdrawArgs("sol.omft.near", 1_500_000_000n, "CBbbB6PNqsW1jNzHebzu3YrKDXQ5UQo9eGbqxvCWvjhG"),
    { token: "sol.omft.near", receiver_id: "sol.omft.near", amount: "1500000000", memo: "WITHDRAW_TO:CBbbB6PNqsW1jNzHebzu3YrKDXQ5UQo9eGbqxvCWvjhG" });
});

test("what to report: the amount less the bridge's fee, accepted only within the owner's fee bound", () => {
  assert.deepEqual(hopReport(1_500_000_000n, 14_000n, 1_000_000n), { out: 1_499_986_000n, accepted: true });
  assert.deepEqual(hopReport(1_000_000n, 30_000n, 10_000n), { out: 970_000n, accepted: false }, "USDC's 0.03 fee over a 0.01 bound");
  assert.deepEqual(hopReport(10n, 20n, 100n), { out: 0n, accepted: true }, "a fee bigger than the hop leaves nothing");
});

test("1Click quotes are always dry runs, with prefixed asset ids and an ISO deadline", () => {
  const b = quoteBody({ from: "wrap.near", to: "nep141:sol.omft.near", amount: 10n ** 24n, refundTo: "v.near", recipient: "v.near", now: new Date("2026-11-06T17:00:00.500Z") });
  assert.equal(b.dry, true);
  assert.equal(b.originAsset, "nep141:wrap.near");
  assert.equal(b.destinationAsset, "nep141:sol.omft.near");
  assert.equal(b.amount, "1000000000000000000000000");
  assert.equal(b.deadline, "2026-11-06T17:30:00Z");
  assert.equal(b.recipientType, "INTENTS");
  assert.equal(asset("1cs_v1:sol:spl:3ZLekZYq2qkZiSpnSvabjit34tUkjSwD1JFuW9as9wBG"), "1cs_v1:sol:spl:3ZLekZYq2qkZiSpnSvabjit34tUkjSwD1JFuW9as9wBG");
  assert.deepEqual(rpcBody("deposit_address", { account_id: "v.near", chain: "sol:mainnet" }),
    { jsonrpc: "2.0", id: 1, method: "deposit_address", params: [{ account_id: "v.near", chain: "sol:mainnet" }] });
});

test("bridge answers: withdrawal status matched by token, terms per token", () => {
  assert.deepEqual(withdrawalState({ error: "Withdrawals not found" }, "sol.omft.near"), { status: "pending" }, "not indexed yet");
  const resp = { result: { withdrawals: [
    { status: "COMPLETED", data: { near_token_id: "base.omft.near", transfer_tx_hash: "0xaa", amount: 5 } },
    { status: "PENDING", data: { near_token_id: "sol.omft.near" } },
  ] } };
  assert.deepEqual(withdrawalState(resp, "base.omft.near"), { status: "completed", txHash: "0xaa", amount: 5 });
  assert.deepEqual(withdrawalState(resp, "sol.omft.near"), { status: "pending" });
  assert.deepEqual(withdrawalState({ result: { withdrawals: [{ status: "FAILED", data: { near_token_id: "x.omft.near" } }] } }, "x.omft.near"), { status: "failed", reason: "FAILED" });
  const terms = { result: { tokens: [{ near_token_id: "sol.omft.near", decimals: 9, min_deposit_amount: "1250000", withdrawal_fee: "14000" }] } };
  assert.deepEqual(tokenTerms(terms, "sol.omft.near"), { decimals: 9, minDeposit: 1_250_000n, withdrawalFee: 14_000n });
  assert.equal(tokenTerms(terms, "base.omft.near"), null);
});
