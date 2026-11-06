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

import { Connection, Keypair, PublicKey, Transaction, TransactionInstruction } from "@solana/web3.js";
import { getAssociatedTokenAddressSync, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import BN from "bn.js";
import bs58 from "bs58";
import { createRequire } from "node:module";
// pump.fun's SDK through its CommonJS build: its ESM build imports `BN` from Anchor's ESM, which doesn't export it.
const require = createRequire(import.meta.url);
const pump = require("@pump-fun/pump-sdk");
const { OnlinePumpSdk, PUMP_SDK, getBuyTokenAmountFromSolAmount } = pump;

const rpc = process.env.SOLANA_RPC ?? "http://127.0.0.1:8899";
const conn = new Connection(rpc, "confirmed");
const [cmd, ...args] = process.argv.slice(2);
const out = (x: unknown) => console.log(JSON.stringify(x));

/** A legacy message (no lookup tables), the vault paying, as base64. */
async function message(payer: PublicKey, ixs: TransactionInstruction[]): Promise<string> {
  const tx = new Transaction();
  tx.add(...ixs);
  tx.feePayer = payer;
  tx.recentBlockhash = (await conn.getLatestBlockhash()).blockhash;
  return tx.serializeMessage().toString("base64");
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
} else if (cmd === "send") {
  const [msg, sigHex, ...secrets] = args;
  const tx = Transaction.populate(require("@solana/web3.js").Message.from(Buffer.from(msg, "base64")));
  tx.addSignature(tx.feePayer!, Buffer.from(sigHex, "hex"));
  for (const s of secrets) tx.partialSign(Keypair.fromSecretKey(bs58.decode(s)));
  try {
    const sig = await conn.sendRawTransaction(tx.serialize(), { skipPreflight: false });
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
