// Solana transactions for the vault's templates, built with the protocols' own
// SDKs against a Solana mainnet fork (surfpool), then sent once signed.
// Used by the sandbox test contracts/sandbox/tests/sol_fork.rs (it prints JSON).
//
//   tsx src/fork-sol.ts pump-info <mint>...                 which coins are still on their bonding curve
//   tsx src/fork-sol.ts pump-buy <vault> <mint> <lamports> <slippage%>
//   tsx src/fork-sol.ts dlmm-open <vault> <owner-secret-b58> <pool> <amountX> <amountY> <bins>
//   tsx src/fork-sol.ts dlmm-claim <vault> <pool> <position>
//   tsx src/fork-sol.ts dlmm-close <vault> <pool> <position>
//   tsx src/fork-sol.ts send <message-b64> <vault-sig-hex> [signer-secret-b58 ...]
//   tsx src/fork-sol.ts balance <owner> <mint>
//
// SOLANA_RPC: the fork (default http://127.0.0.1:8899).

import { Connection, Keypair, Message, PublicKey, Transaction, TransactionInstruction } from "@solana/web3.js";
import { getAssociatedTokenAddressSync, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import BN from "bn.js";
import bs58 from "bs58";
import { createRequire } from "node:module";
// pump.fun's SDK through its CommonJS build: its ESM build imports `BN` from Anchor's ESM, which doesn't export it.
const require = createRequire(import.meta.url);
const pump = require("@pump-fun/pump-sdk");
const { OnlinePumpSdk, PUMP_SDK, getBuyTokenAmountFromSolAmount, getSellSolAmountFromTokenAmount, OnlinePumpAmmSdk, PUMP_AMM_SDK } = pump;

const rpc = process.env.SOLANA_RPC ?? "http://127.0.0.1:8899";
const conn = new Connection(rpc, "confirmed");
const [cmd, ...args] = process.argv.slice(2);
const out = (x: unknown) => console.log(JSON.stringify(x));

/** A legacy message (no lookup tables), the vault paying, as base64. `cosigners` sign without
 * being named by any instruction (the owner approving an LP open): web3.js refuses such
 * "unknown signers", so they're spliced in as read-only signers after the compiled ones. */
async function message(payer: PublicKey, ixs: TransactionInstruction[], cosigners: PublicKey[] = []): Promise<string> {
  const tx = new Transaction();
  tx.add(...ixs);
  tx.feePayer = payer;
  tx.recentBlockhash = (await conn.getLatestBlockhash()).blockhash;
  const m = tx.compileMessage();
  const add = cosigners.filter((c) => !m.accountKeys.some((k) => k.equals(c)));
  if (add.length === 0) return m.serialize().toString("base64");
  const at = m.header.numRequiredSignatures;
  const shift = (i: number) => (i >= at ? i + add.length : i);
  const spliced = new Message({
    header: {
      numRequiredSignatures: at + add.length,
      numReadonlySignedAccounts: m.header.numReadonlySignedAccounts + add.length,
      numReadonlyUnsignedAccounts: m.header.numReadonlyUnsignedAccounts,
    },
    accountKeys: [...m.accountKeys.slice(0, at), ...add, ...m.accountKeys.slice(at)].map((k) => k.toBase58()),
    recentBlockhash: m.recentBlockhash,
    instructions: m.instructions.map((ix) => ({ programIdIndex: shift(ix.programIdIndex), accounts: ix.accounts.map(shift), data: ix.data })),
  });
  return spliced.serialize().toString("base64");
}

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

const describe = (ixs: TransactionInstruction[]) => ixs.map((i) => ({ program: i.programId.toBase58(), accounts: i.keys.length, data: i.data.toString("hex").slice(0, 16) }));

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
  const [vault, mint, lamports, slippage] = args;
  const user = new PublicKey(vault), coin = new PublicKey(mint);
  const sdk = new OnlinePumpSdk(conn);
  const global = await sdk.fetchGlobal();
  const feeConfig = await sdk.fetchFeeConfig();
  const tokenProgram = (await conn.getAccountInfo(coin))!.owner;
  const state = await sdk.fetchBuyState(coin, user, tokenProgram);
  const mintInfo = await conn.getTokenSupply(coin);
  const amount = getBuyTokenAmountFromSolAmount({ global, feeConfig, mintSupply: new BN(mintInfo.value.amount), bondingCurve: state.bondingCurve, amount: new BN(lamports) } as any);
  const ixs = await PUMP_SDK.buyInstructions({ global, bondingCurveAccountInfo: state.bondingCurveAccountInfo, bondingCurve: state.bondingCurve,
    associatedUserAccountInfo: state.associatedUserAccountInfo, mint: coin, user, amount, solAmount: new BN(lamports), slippage: Number(slippage), tokenProgram });
  out({ message: await message(user, ixs), tokens: amount.toString(), tokenProgram: tokenProgram.toBase58(), ata: getAssociatedTokenAddressSync(coin, user, true, tokenProgram).toBase58(), ixs: describe(ixs) });
} else if (cmd === "pump-sell") {
  // Sell `amount` coin units back to the curve; SOL out lands in the vault itself.
  const [vault, mint, amount, slippage] = args;
  const user = new PublicKey(vault), coin = new PublicKey(mint);
  const sdk = new OnlinePumpSdk(conn);
  const global = await sdk.fetchGlobal();
  const feeConfig = await sdk.fetchFeeConfig();
  const tokenProgram = (await conn.getAccountInfo(coin))!.owner;
  const state = await sdk.fetchSellState(coin, user, tokenProgram);
  const mintInfo = await conn.getTokenSupply(coin);
  const sol = getSellSolAmountFromTokenAmount({ global, feeConfig, mintSupply: new BN(mintInfo.value.amount), bondingCurve: state.bondingCurve, amount: new BN(amount) } as any);
  const ixs = await PUMP_SDK.sellInstructions({ global, bondingCurveAccountInfo: state.bondingCurveAccountInfo, bondingCurve: state.bondingCurve,
    mint: coin, user, amount: new BN(amount), solAmount: sol, slippage: Number(slippage), tokenProgram });
  out({ message: await message(user, ixs), lamports: sol.toString(), tokenProgram: tokenProgram.toBase58(), ixs: describe(ixs) });
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
