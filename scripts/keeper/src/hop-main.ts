// Hop helper: read-only lookups and dry-run quotes for moving a vault's money between chains
// through NEAR Intents (see hop.ts for the formats and the list of supported chains, and
// scripts/HOP.md for the whole hop). Nothing here signs or sends: no keys, no funds.
//
//   npm run hop -- tokens [blockchain ...]               1Click's supported tokens (e.g. sol base hood)
//   npm run hop -- terms <chain> [near-token]            the bridge's decimals, min deposit, withdrawal fee (chain: sol:mainnet, eth:8453, eth:4663)
//   npm run hop -- deposit-address <account> <chain>     the account's Intents deposit address on a chain (the owner pins it in a deposit template)
//   npm run hop -- deposits <account> <chain>            recent deposits the bridge saw for the account
//   npm run hop -- quote <from> <to> <amount> <account> [intents|chain:<recipient>]   1Click dry-run quote (dry: true)
//   npm run hop -- status <near-tx-hash> <near-token> [--wait]   a hop's withdrawal on the bridge; with --wait, poll until it lands
//   npm run hop -- route <near-token>                    where a hop of this token lands, and the memo shape

import { bridge, bridgeChain, hopChainOf, landsAs, oneClick, quoteBody, tokenTerms, withdrawalState, withdrawMemo } from "./hop.ts";

const [cmd, ...args] = process.argv.slice(2);
const out = (x: unknown) => console.log(JSON.stringify(x, null, 2));
const need = (n: number, usage: string) => {
  if (args.length < n) {
    console.error(`usage: npm run hop -- ${usage}`);
    process.exit(2);
  }
};

if (cmd === "tokens") {
  const all: any[] = await oneClick("/v0/tokens");
  const pick = args.length ? all.filter((t) => args.includes(t.blockchain)) : all;
  const chains = [...new Set(all.map((t) => t.blockchain))].sort();
  out({ chains, tokens: pick.map((t) => ({ blockchain: t.blockchain, symbol: t.symbol, assetId: t.assetId, decimals: t.decimals, contract: t.contractAddress ?? null })) });
} else if (cmd === "terms") {
  need(1, "terms <chain> [near-token]");
  const r = await bridge("supported_tokens", { chains: [args[0]] });
  if (args[1]) {
    const t = tokenTerms(r, args[1]);
    out(t ? { ...t, minDeposit: t.minDeposit.toString(), withdrawalFee: t.withdrawalFee.toString() } : { error: "not served on that chain" });
  } else {
    out((r.result?.tokens ?? []).map((t: any) => ({ token: t.near_token_id, name: t.asset_name, decimals: t.decimals, min_deposit: t.min_deposit_amount, withdrawal_fee: t.withdrawal_fee })));
  }
} else if (cmd === "deposit-address") {
  need(2, "deposit-address <account> <chain>");
  out(await bridge("deposit_address", { account_id: args[0], chain: args[1] }));
} else if (cmd === "deposits") {
  need(2, "deposits <account> <chain>");
  out(await bridge("recent_deposits", { account_id: args[0], chain: args[1] }));
} else if (cmd === "quote") {
  need(4, "quote <from> <to> <amount> <account> [intents|chain:<recipient>]");
  const [from, to, amount, account, where = "intents"] = args;
  const toChain = where.startsWith("chain:");
  const body = quoteBody({ from, to, amount: BigInt(amount), refundTo: account, recipient: toChain ? where.slice(6) : account, recipientType: toChain ? "DESTINATION_CHAIN" : "INTENTS" });
  out(await oneClick("/v0/quote", body));
} else if (cmd === "status") {
  need(2, "status <near-tx-hash> <near-token> [--wait]");
  const [hash, token] = args;
  for (;;) {
    const s = withdrawalState(await bridge("withdrawal_status", { withdrawal_hash: hash }), token);
    if (!args.includes("--wait") || s.status !== "pending") {
      out(s);
      break;
    }
    console.error("pending...");
    await new Promise((r) => setTimeout(r, 15_000));
  }
} else if (cmd === "route") {
  need(1, "route <near-token>");
  const c = hopChainOf(args[0]);
  out(c ? { lands_on: bridgeChain(c), report_as: `${landsAs(c)}...`, memo: withdrawMemo("<the vault's own address there>") } : { error: "not a POA bridge token a hop serves" });
} else {
  console.error("commands: tokens, terms, deposit-address, deposits, quote, status, route (see the header of src/hop-main.ts)");
  process.exit(2);
}
