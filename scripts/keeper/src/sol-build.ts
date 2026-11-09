// Solana messages for the vault's templates, built with the protocols' own SDKs: shared by
// the fork helper (fork-sol.ts, the sandbox tests) and the flow keeper's remote nodes
// (remote-run.ts). Each returns a legacy message (no lookup tables), the vault paying, base64.

import { Connection, Message, PublicKey, SystemProgram, Transaction, TransactionInstruction } from "@solana/web3.js";
import { createTransferCheckedInstruction, getAssociatedTokenAddressSync } from "@solana/spl-token";
import BN from "bn.js";
import { createRequire } from "node:module";
// pump.fun's SDK through its CommonJS build: its ESM build imports `BN` from Anchor's ESM, which doesn't export it.
const require = createRequire(import.meta.url);
const pump = require("@pump-fun/pump-sdk");
const { OnlinePumpSdk, PUMP_SDK, getBuyTokenAmountFromSolAmount, getSellSolAmountFromTokenAmount } = pump;

/** A legacy message (no lookup tables), the vault paying, as base64. `cosigners` sign without
 * being named by any instruction (the owner approving an LP open): web3.js refuses such
 * "unknown signers", so they're spliced in as read-only signers after the compiled ones. */
export async function message(conn: Connection, payer: PublicKey, ixs: TransactionInstruction[], cosigners: PublicKey[] = []): Promise<string> {
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

export const describe = (ixs: TransactionInstruction[]) => ixs.map((i) => ({ program: i.programId.toBase58(), accounts: i.keys.length, data: i.data.toString("hex").slice(0, 16) }));

/** A pump.fun buy of `mint` for `lamports`. Default: buy what `lamports` quotes, paying up to
 * slippage% more. exact: pay at most `lamports` exactly (a flow node must spend exactly its
 * input) and take slippage% fewer coins. */
export async function pumpBuy(conn: Connection, vault: string, mint: string, lamports: bigint | string, slippagePct: number, exact: boolean) {
  const user = new PublicKey(vault), coin = new PublicKey(mint);
  const sdk = new OnlinePumpSdk(conn);
  const global = await sdk.fetchGlobal();
  const feeConfig = await sdk.fetchFeeConfig();
  const tokenProgram = (await conn.getAccountInfo(coin))!.owner;
  const state = await sdk.fetchBuyState(coin, user, tokenProgram);
  const mintInfo = await conn.getTokenSupply(coin);
  const quoted: BN = getBuyTokenAmountFromSolAmount({ global, feeConfig, mintSupply: new BN(mintInfo.value.amount), bondingCurve: state.bondingCurve, amount: new BN(lamports.toString()) } as any);
  const amount = exact ? quoted.muln(100 - slippagePct).divn(100) : quoted;
  const ixs: TransactionInstruction[] = await PUMP_SDK.buyInstructions({ global, bondingCurveAccountInfo: state.bondingCurveAccountInfo, bondingCurve: state.bondingCurve,
    associatedUserAccountInfo: state.associatedUserAccountInfo, mint: coin, user, amount, solAmount: new BN(lamports.toString()), slippage: exact ? 0 : slippagePct, tokenProgram });
  return { message: await message(conn, user, ixs), tokens: amount.toString(), tokenProgram: tokenProgram.toBase58(), ata: getAssociatedTokenAddressSync(coin, user, true, tokenProgram).toBase58(), ixs: describe(ixs) };
}

/** Sell `units` coin units back to the curve; SOL out lands in the vault itself, at least the quote less slippage%. */
export async function pumpSell(conn: Connection, vault: string, mint: string, units: bigint | string, slippagePct: number) {
  const user = new PublicKey(vault), coin = new PublicKey(mint);
  const sdk = new OnlinePumpSdk(conn);
  const global = await sdk.fetchGlobal();
  const feeConfig = await sdk.fetchFeeConfig();
  const tokenProgram = (await conn.getAccountInfo(coin))!.owner;
  const state = await sdk.fetchSellState(coin, user, tokenProgram);
  const mintInfo = await conn.getTokenSupply(coin);
  const sol: BN = getSellSolAmountFromTokenAmount({ global, feeConfig, mintSupply: new BN(mintInfo.value.amount), bondingCurve: state.bondingCurve, amount: new BN(units.toString()) } as any);
  const ixs: TransactionInstruction[] = await PUMP_SDK.sellInstructions({ global, bondingCurveAccountInfo: state.bondingCurveAccountInfo, bondingCurve: state.bondingCurve,
    mint: coin, user, amount: new BN(units.toString()), solAmount: sol, slippage: slippagePct, tokenProgram,
    // The SDK's buy reads this from the curve, its sell defaults to false: a "mayhem mode" coin's
    // fees go to other recipients, and the program refuses the wrong one (NotAuthorized).
    mayhemMode: state.bondingCurve.isMayhemMode });
  return { message: await message(conn, user, ixs), lamports: sol.toString(), minLamports: sol.muln(100 - slippagePct).divn(100).toString(), tokenProgram: tokenProgram.toBase58(), ixs: describe(ixs) };
}

/** A System transfer (Intents deposits). */
export async function paySol(conn: Connection, from: string, to: string, lamports: bigint | string) {
  const ix = SystemProgram.transfer({ fromPubkey: new PublicKey(from), toPubkey: new PublicKey(to), lamports: BigInt(lamports) });
  return { message: await message(conn, new PublicKey(from), [ix]) };
}

/** An SPL TransferChecked from the vault's own associated token account to a given token account. */
export async function paySpl(conn: Connection, from: string, to: string, mint: string, units: bigint | string, decimals: number) {
  const owner = new PublicKey(from), m = new PublicKey(mint);
  const ix = createTransferCheckedInstruction(getAssociatedTokenAddressSync(m, owner, true), m, new PublicKey(to), owner, BigInt(units), decimals);
  return { message: await message(conn, owner, [ix]) };
}
