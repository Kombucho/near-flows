// Solana transactions for the vault's templates, built with the protocols' own
// SDKs against a Solana mainnet fork (surfpool), then sent once signed.
// Used by the sandbox tests contracts/sandbox/tests/sol_fork*.rs (it prints JSON).
//
//   tsx src/fork-sol.ts pump-info <mint>...                 which coins are still on their bonding curve
//   tsx src/fork-sol.ts pump-buy <vault> <mint> <lamports> <slippage%> [exact]
//   tsx src/fork-sol.ts pump-sell <vault> <mint> <units> <slippage%>
//   tsx src/fork-sol.ts amm-info <mint>...                  a graduated coin's canonical pump AMM pool
//   tsx src/fork-sol.ts amm-buy <vault> <mint> <lamports> <slippage%>
//   tsx src/fork-sol.ts amm-sell <vault> <mint> <units> <slippage%>
//   tsx src/fork-sol.ts dlmm-info <pool>...
//   tsx src/fork-sol.ts dlmm-open <vault> <owner-pubkey|-> <pool> <amountY> <bins>
//   tsx src/fork-sol.ts dlmm-swap <trader-secret-b58> <pool> <lamports>   a trader sells SOL (signed and sent here)
//   tsx src/fork-sol.ts dlmm-position <pool> <position>
//   tsx src/fork-sol.ts dlmm-claim <vault> <pool> <position> [unwrap]
//   tsx src/fork-sol.ts dlmm-close <vault> <pool> <position>
//   tsx src/fork-sol.ts pay-sol <vault> <to> <lamports>                  a System transfer (Intents deposits)
//   tsx src/fork-sol.ts pay-spl <vault> <to-token-account> <mint> <units> <decimals>   an SPL TransferChecked
//   tsx src/fork-sol.ts send <message-b64> <vault-sig-hex> [signer-secret-b58 ...]
//   tsx src/fork-sol.ts swap-key <message-b64> <from> <to>   a tampered copy (refusal tests)
//   tsx src/fork-sol.ts balance <owner> <mint> | ata <owner> <mint> [program] | keypair | sign <message-b64> <secret> | decode <message-b64>
//
// SOLANA_RPC: the fork (default http://127.0.0.1:8899).

import { Connection, Keypair, Message, PublicKey, Transaction, TransactionInstruction } from "@solana/web3.js";
import { createCloseAccountInstruction, getAssociatedTokenAddressSync, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import * as build from "./sol-build.ts";
import BN from "bn.js";
import bs58 from "bs58";
import { createRequire } from "node:module";
// pump.fun's SDK through its CommonJS build: its ESM build imports `BN` from Anchor's ESM, which doesn't export it.
const require = createRequire(import.meta.url);
const pump = require("@pump-fun/pump-sdk");
const { OnlinePumpSdk } = pump;
// The pump AMM (graduated coins): its own SDK (pump-sdk bundles it but does not export it).
const pumpAmm = require("@pump-fun/pump-swap-sdk");
const { OnlinePumpAmmSdk, PUMP_AMM_SDK } = pumpAmm;

const rpc = process.env.SOLANA_RPC ?? "http://127.0.0.1:8899";
const conn = new Connection(rpc, "confirmed");
const [cmd, ...args] = process.argv.slice(2);
const out = (x: unknown) => console.log(JSON.stringify(x));

/** A legacy message, the vault paying, as base64 (sol-build.ts). */
const message = (payer: PublicKey, ixs: TransactionInstruction[], cosigners: PublicKey[] = []) => build.message(conn, payer, ixs, cosigners);

// Meteora's SDK, CommonJS build for the same reason as pump.fun's. No SOL wrapping: the vault
// deposits one-sided in an SPL token, and an unwrap would close the account its fees land in.
const DLMM = require("@meteora-ag/dlmm");
const dlmm = (pool: string) => DLMM.create(conn, new PublicKey(pool), { skipSolWrappingOperation: true });

/** Drop "create token account" instructions for accounts that already exist (the SDKs add them
 * just in case): a smaller transaction, and nothing in it the vault doesn't need to check. */
async function trim(ixs: TransactionInstruction[]): Promise<TransactionInstruction[]> {
  const ata = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
  const kept = [];
  for (const ix of ixs) {
    if (ix.programId.toBase58() === ata && (await conn.getAccountInfo(ix.keys[1].pubkey)) !== null) continue;
    kept.push(ix);
  }
  return kept;
}

const describe = build.describe;

if (cmd === "pump-info") {
  const sdk = new OnlinePumpSdk(conn);
  const res = [];
  for (const m of args) {
    try {
      const s = await sdk.fetchBuyState(new PublicKey(m), Keypair.generate().publicKey);
      res.push({ mint: m, quote: s.quoteMint?.toBase58?.(), complete: s.bondingCurve.complete, virtualSol: s.bondingCurve.virtualQuoteReserves?.toString?.() ?? s.bondingCurve.virtualSolReserves?.toString(), program: s.bondingCurveAccountInfo.owner.toBase58() });
    } catch (e) {
      res.push({ mint: m, error: (e as Error).message.slice(0, 80) });
    }
  }
  out(res);
} else if (cmd === "pump-buy") {
  // Default: buy what `lamports` quotes, paying up to slippage% more. "exact": pay at most
  // `lamports` exactly (a flow node must spend exactly its input) and take slippage% fewer coins.
  const [vault, mint, lamports, slippage, mode] = args;
  out(await build.pumpBuy(conn, vault, mint, lamports, Number(slippage), mode === "exact"));
} else if (cmd === "pump-sell") {
  // Sell `amount` coin units back to the curve; SOL out lands in the vault itself.
  const [vault, mint, amount, slippage] = args;
  const { minLamports: _, ...res } = await build.pumpSell(conn, vault, mint, amount, Number(slippage));
  out(res);
} else if (cmd === "amm-info") {
  // A graduated coin's canonical pump AMM pool: its quote token and reserves.
  const amm = new OnlinePumpAmmSdk(conn);
  const res = [];
  for (const m of args) {
    try {
      const key = pumpAmm.canonicalPumpPoolPda(new PublicKey(m));
      const p = await amm.fetchPool(key);
      const [b, q] = await Promise.all([conn.getTokenAccountBalance(p.poolBaseTokenAccount), conn.getTokenAccountBalance(p.poolQuoteTokenAccount)]);
      res.push({ mint: m, pool: key.toBase58(), quote: p.quoteMint.toBase58(), base: b.value.amount, quoteReserve: q.value.amount });
    } catch (e) {
      res.push({ mint: m, error: (e as Error).message.slice(0, 80) });
    }
  }
  out(res);
} else if (cmd === "amm-buy" || cmd === "amm-sell") {
  // A graduated coin on the pump AMM, against wSOL: buy with `amount` lamports, or sell `amount`
  // coin units. The SDK wraps SOL into the vault's own wSOL account around the swap and closes
  // it after (the SOL back to the vault).
  const [vault, mint, amount, slippage] = args;
  const user = new PublicKey(vault);
  const amm = new OnlinePumpAmmSdk(conn);
  const state = await amm.swapSolanaState(pumpAmm.canonicalPumpPoolPda(new PublicKey(mint)), user);
  const ixs: TransactionInstruction[] = cmd === "amm-buy"
    ? await PUMP_AMM_SDK.buyQuoteInput(state, new BN(amount), Number(slippage))
    : await PUMP_AMM_SDK.sellBaseInput(state, new BN(amount), Number(slippage));
  // The same inputs the SDK quotes with inside buyQuoteInput / sellBaseInput.
  const q = { slippage: Number(slippage), baseReserve: state.poolBaseAmount, quoteReserve: state.poolQuoteAmount, virtualQuoteReserves: state.pool.virtualQuoteReserves,
    globalConfig: state.globalConfig, baseMintAccount: state.baseMintAccount, baseMint: state.baseMint, coinCreator: state.pool.coinCreator, creator: state.pool.creator,
    feeConfig: state.feeConfig, quoteMint: state.pool.quoteMint, isMayhemMode: state.pool.isMayhemMode, creatorFeeBps: state.pool.creatorFeeBps, quoteTransferFee: state.quoteTransferFee };
  const quote = cmd === "amm-buy" ? pumpAmm.buyQuoteInput({ ...q, quote: new BN(amount) }).base : pumpAmm.sellBaseInput({ ...q, base: new BN(amount) }).uiQuote;
  const trimmed = await trim(ixs);
  out({ message: await message(user, trimmed), out: quote.toString(), tokenProgram: state.baseTokenProgram.toBase58(), ixs: describe(trimmed) });
} else if (cmd === "dlmm-info") {
  const res = [];
  for (const p of args) {
    try {
      const d = await dlmm(p);
      const bin = await d.getActiveBin();
      res.push({ pool: p, x: d.tokenX.publicKey.toBase58(), y: d.tokenY.publicKey.toBase58(), xProgram: d.tokenX.owner.toBase58(), yProgram: d.tokenY.owner.toBase58(),
        binStep: d.lbPair.binStep, activeId: d.lbPair.activeId, price: bin.pricePerToken, reserveX: d.tokenX.amount?.toString(), reserveY: d.tokenY.amount?.toString() });
    } catch (e) {
      res.push({ pool: p, error: (e as Error).message.slice(0, 120) });
    }
  }
  out(res);
} else if (cmd === "dlmm-open") {
  // A new position over the `bins` bins at and below the active one, filled with token Y only
  // (USDC in SOL/USDC): a bid ladder, no SOL to wrap. The vault pays and owns the position; a
  // fresh key is its address; the owner's wallet ("-": none) co-signs to approve the range.
  const [vault, owner, pool, amountY, bins] = args;
  const user = new PublicKey(vault), d = await dlmm(pool), position = Keypair.generate();
  const active = d.lbPair.activeId;
  const minBinId = active - Number(bins) + 1, maxBinId = active;
  const tx = await d.initializePositionAndAddLiquidityByStrategy({ positionPubKey: position.publicKey, user, totalXAmount: new BN(0), totalYAmount: new BN(amountY),
    strategy: { minBinId, maxBinId, strategyType: 0 /* Spot */ }, slippage: 1 });
  const ixs: TransactionInstruction[] = tx.instructions;
  const cosigners = owner === "-" ? [] : [new PublicKey(owner)];
  out({ message: await message(user, ixs, cosigners), position: position.publicKey.toBase58(), positionSecret: bs58.encode(position.secretKey), minBinId, maxBinId, ixs: describe(ixs) });
} else if (cmd === "dlmm-swap") {
  // A trader (a fork-funded key, not the vault) sells `lamports` of SOL into the pool, so the
  // vault's bins earn fees. Signed and sent here.
  const [secret, pool, lamports] = args;
  const trader = Keypair.fromSecretKey(bs58.decode(secret));
  const d = await DLMM.create(conn, new PublicKey(pool));
  const arrays = await d.getBinArrayForSwap(true);
  const q = d.swapQuote(new BN(lamports), true, new BN(100), arrays);
  const tx: Transaction = await d.swap({ inToken: d.tokenX.publicKey, outToken: d.tokenY.publicKey, inAmount: new BN(lamports), minOutAmount: q.minOutAmount,
    lbPair: d.pubkey, user: trader.publicKey, binArraysPubkey: q.binArraysPubkey });
  tx.recentBlockhash = (await conn.getLatestBlockhash()).blockhash;
  tx.feePayer = trader.publicKey;
  tx.sign(trader);
  const sig = await conn.sendRawTransaction(tx.serialize());
  const st = await conn.confirmTransaction(sig, "confirmed");
  out({ signature: sig, err: st.value.err, out: q.outAmount.toString(), activeAfter: (await dlmm(pool)).lbPair.activeId });
} else if (cmd === "dlmm-position") {
  const [pool, position] = args;
  const d = await dlmm(pool);
  const p = await d.getPosition(new PublicKey(position));
  const x = p.positionData;
  out({ x: x.totalXAmount.toString(), y: x.totalYAmount.toString(), feeX: x.feeX.toString(), feeY: x.feeY.toString(), lower: x.lowerBinId, upper: x.upperBinId });
} else if (cmd === "dlmm-claim") {
  const [vault, pool, position] = args;
  const user = new PublicKey(vault), d = await dlmm(pool);
  const txs: Transaction[] = await d.claimSwapFee({ owner: user, position: await d.getPosition(new PublicKey(position)) });
  const ixs = await trim(txs.flatMap((t) => t.instructions));
  // "unwrap": close the vault's wSOL account in the same transaction, so the SOL fees land as
  // plain SOL in the vault (what a pump.fun buy spends). Its rent comes back to the vault too.
  if (args[3] === "unwrap") {
    const wsol = new PublicKey("So11111111111111111111111111111111111111112");
    ixs.push(createCloseAccountInstruction(getAssociatedTokenAddressSync(wsol, user, true), user, user));
  }
  out({ message: await message(user, ixs), ixs: describe(ixs) });
} else if (cmd === "dlmm-close") {
  // Withdraw everything, claim the fees, close the position (its rent back to the vault).
  const [vault, pool, position] = args;
  const user = new PublicKey(vault), d = await dlmm(pool);
  const p = await d.getPosition(new PublicKey(position));
  const txs: Transaction[] = await d.removeLiquidity({ user, position: new PublicKey(position), fromBinId: p.positionData.lowerBinId, toBinId: p.positionData.upperBinId,
    bps: new BN(10_000), shouldClaimAndClose: true, skipUnwrapSOL: true });
  const ixs = await trim(txs.flatMap((t) => t.instructions));
  out({ message: await message(user, ixs), txs: txs.length, ixs: describe(ixs) });
} else if (cmd === "pay-sol") {
  const [from, to, lamports] = args;
  out(await build.paySol(conn, from, to, lamports));
} else if (cmd === "pay-spl") {
  // From the vault's own associated token account to a given token account, mint and decimals checked.
  const [from, to, mint, units, decimals] = args;
  out(await build.paySpl(conn, from, to, mint, units, Number(decimals)));
} else if (cmd === "swap-key") {
  // A tampered copy of a message: every use of one account key replaced by another (refusal tests).
  const [msg, from, to] = args;
  const m = Message.from(Buffer.from(msg, "base64"));
  const keys = m.accountKeys.map((k) => (k.toBase58() === from ? new PublicKey(to) : k));
  out({ message: new Message({ header: m.header, accountKeys: keys.map((k) => k.toBase58()), recentBlockhash: m.recentBlockhash, instructions: m.instructions }).serialize().toString("base64") });
} else if (cmd === "ata") {
  const [owner, mint, program] = args;
  out({ ata: getAssociatedTokenAddressSync(new PublicKey(mint), new PublicKey(owner), true, program ? new PublicKey(program) : TOKEN_PROGRAM_ID).toBase58() });
} else if (cmd === "sign") {
  // A local key's signature over a message, hex: stands in for the vault when replaying by hand.
  const [msg, secret] = args;
  const nacl = require("tweetnacl");
  out({ signature: Buffer.from(nacl.sign.detached(Buffer.from(msg, "base64"), bs58.decode(secret))).toString("hex") });
} else if (cmd === "keypair") {
  const k = Keypair.generate();
  out({ pubkey: k.publicKey.toBase58(), secret: bs58.encode(k.secretKey) });
} else if (cmd === "send") {
  // The wire transaction is assembled here, signatures in the header's signer order then the
  // exact message bytes the vault signed: web3.js would recompile the message (and refuse a
  // signer no instruction names, like the owner approving an LP open). A signer with no secret
  // given keeps an empty signature, so the chain refuses the transaction.
  const [msg, sigHex, ...secrets] = args;
  const bytes = Buffer.from(msg, "base64");
  const m = Message.from(bytes);
  const nacl = require("tweetnacl");
  const keys = secrets.map((s) => Keypair.fromSecretKey(bs58.decode(s)));
  const sigs = m.accountKeys.slice(0, m.header.numRequiredSignatures).map((k, i) => {
    if (i === 0) return Buffer.from(sigHex, "hex");
    const kp = keys.find((x) => x.publicKey.equals(k));
    return kp ? Buffer.from(nacl.sign.detached(bytes, kp.secretKey)) : Buffer.alloc(64);
  });
  const wire = Buffer.concat([Buffer.from([sigs.length]), ...sigs, bytes]);
  try {
    const sig = await conn.sendRawTransaction(wire, { skipPreflight: false });
    const st = await conn.confirmTransaction(sig, "confirmed");
    out({ signature: sig, err: st.value.err });
  } catch (e) {
    out({ error: (e as Error).message.slice(0, 600), logs: (e as any).logs?.slice(-8) });
  }
} else if (cmd === "decode") {
  const { Message } = require("@solana/web3.js");
  const m = Message.from(Buffer.from(args[0], "base64"));
  out({ signers: m.header.numRequiredSignatures, keys: m.accountKeys.map((k: PublicKey) => k.toBase58()),
    ixs: m.instructions.map((ix: any) => { const d = Buffer.from(bs58.decode(ix.data)); return { program: m.accountKeys[ix.programIdIndex].toBase58(), accounts: ix.accounts, len: d.length, data: d.toString("hex") }; }) });
} else if (cmd === "balance") {
  const [owner, mint] = args;
  const accs = await conn.getParsedTokenAccountsByOwner(new PublicKey(owner), { mint: new PublicKey(mint) });
  out({ amount: accs.value.reduce((s, a) => s + BigInt(a.account.data.parsed.info.tokenAmount.amount), 0n).toString(), lamports: await conn.getBalance(new PublicKey(owner)) });
} else {
  throw new Error(`unknown command ${cmd}`);
}
