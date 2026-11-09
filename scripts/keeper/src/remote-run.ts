// The flow keeper's remote nodes, end to end (flows-main.ts --send --remote):
//   step:   build the template's values / Solana message (remote.ts plans, sol-build.ts),
//           ask the vault to sign it (step_remote), keep what it signed in the state file;
//   send:   broadcast it to the chain's RPC (EVM_RPC_<chainid>, SOLANA_RPC); a hop is sent by NEAR;
//   report: measure what landed at the vault's own address there and call remote_report with
//           it (between the signed minimum and the node's cap).
// Nothing is guessed: a template with no builder, a chain with no RPC, a nonce that doesn't match
// the chain, a fee under the base fee: skipped and logged. A step that can't land (reverted,
// refused) or can't be seen is never reported failed (only the owner may): it's logged for the owner.
// In-flight steps live in ~/.flows/keeper/<keeper>.json, so a restart resumes them, never re-sends.

import { Connection, PublicKey } from "@solana/web3.js";
import { getAssociatedTokenAddressSync } from "@solana/spl-token";
import bs58 from "bs58";
import { keccak_256 } from "@noble/hashes/sha3.js";
import { Chain, MAX_TGAS } from "./chain.ts";
import { bridge, hopChainOf, withdrawalState } from "./hop.ts";
import {
  amountOf, balanceOfCalldata, drop, evmArgs, evmPlan, evmValues, lessSlippage, loadState, nextFor, orphans, parseToken, reportable,
  saveState, solPlan, stepResult, upsert, v4QuoteCalldata, keyOf,
  type EvmConfig, type InFlight, type KeeperState, type Pending, type Remote, type RemoteToken, type SolConfig, type Trigger,
} from "./remote.ts";
import * as build from "./sol-build.ts";
import { withSignature } from "./solana.ts";

/** The Uniswap v4 quoter per chain (nodes/evm-templates.json, robinhood.uniswap_v4.quoter); V4_QUOTER_<chainid> overrides. */
const TOKEN_PROGRAM = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const TOKEN_2022 = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
const V4_QUOTERS: Record<number, string> = { 4663: "0x8dc178efb8111bb0973dd9d722ebeff267c98f94" };

export type RemoteOpts = {
  log: (s: string) => void;
  statePath: string;
  /** Slippage under a quote the keeper signs for: EVM in bps (REMOTE_SLIPPAGE_BPS, default 100), Solana pump.fun in % (SOL_SLIPPAGE_PCT, default 5). */
  slippageBps: number;
  solSlippagePct: number;
  /** How long to wait for a hop to land (HOP_WAIT_MIN, default 180) and a transaction to confirm (REMOTE_WAIT_MIN, default 30) before telling the owner. */
  hopWaitMs: number;
  txWaitMs: number;
  /** Ask the bridge API about a hop's withdrawal too (HOP_BRIDGE=1; on by default on mainnet). */
  bridgeStatus: boolean;
  env: Record<string, string | undefined>;
};

export function optsFromEnv(statePath: string, log: (s: string) => void, env = process.env): RemoteOpts {
  const n = (k: string, d: number) => (env[k] !== undefined && env[k] !== "" ? Number(env[k]) : d);
  return {
    log, statePath, env,
    slippageBps: n("REMOTE_SLIPPAGE_BPS", 100),
    solSlippagePct: n("SOL_SLIPPAGE_PCT", 5),
    hopWaitMs: n("HOP_WAIT_MIN", 180) * 60_000,
    txWaitMs: n("REMOTE_WAIT_MIN", 30) * 60_000,
    bridgeStatus: env.HOP_BRIDGE !== undefined ? env.HOP_BRIDGE === "1" : (env.NEAR_NETWORK ?? "testnet") === "mainnet",
  };
}

// ---------- the other chains ----------

class EvmRpc {
  constructor(readonly url: string) {}
  async call<T = any>(method: string, params: unknown[]): Promise<T> {
    const r = await fetch(this.url, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }) });
    const j = await r.json();
    if (j.error) throw new Error(`${method}: ${j.error.message ?? JSON.stringify(j.error)}`);
    return j.result as T;
  }
  async balance(who: string, token: string, block: string = "latest"): Promise<bigint> {
    if (token === "eth") return BigInt(await this.call<string>("eth_getBalance", [who, block]));
    return BigInt(await this.call<string>("eth_call", [{ to: token, data: balanceOfCalldata(who) }, block]));
  }
}

const hex = (n: bigint | number) => "0x" + BigInt(n).toString(16);
const short = (e: unknown) => ((e as Error).message ?? String(e)).replace(/\s+/g, " ").slice(0, 220);

/** The node's output there, with the chain's own client. */
type Out = { token: RemoteToken | null };

// ---------- the runner ----------

export type VaultFull = { owner: string; keeper: string | null; evm?: EvmConfig | null; sol?: SolConfig | null; remote?: Pending[] };
export type RemoteNode = { input: string; when: Trigger; action: { label: string; remote?: Remote | null } };

export class RemoteRunner {
  private state: KeeperState;
  private told = new Set<string>(); // owner messages already logged this run

  constructor(readonly chain: Chain, readonly brain: string, readonly o: RemoteOpts) {
    this.state = loadState(o.statePath);
  }

  get inflight() {
    return this.state.inflight;
  }

  private save(f?: InFlight, gone = false) {
    if (f) this.state = gone ? drop(this.state, f) : upsert(this.state, f);
    saveState(this.o.statePath, this.state);
  }

  private evm(chainId: number): EvmRpc | null {
    const u = this.o.env[`EVM_RPC_${chainId}`];
    return u ? new EvmRpc(u) : null;
  }

  private sol(): Connection | null {
    const u = this.o.env.SOLANA_RPC;
    return u ? new Connection(u, "confirmed") : null;
  }

  /** Which RPC a node needs, or why it can't run. Default: none set, nothing leaves NEAR. */
  needs(r: Remote, vs: VaultFull): string | null {
    const c = r.call;
    if (c.chain === "evm") {
      const id = vs.evm?.chain_id;
      return id === undefined ? "the vault has no EVM setup" : this.evm(id) ? null : `set EVM_RPC_${id} to run it`;
    }
    if (c.chain === "sol") return this.sol() ? null : "set SOLANA_RPC to run it";
    const lands = r.out ? parseToken(r.out) : null;
    if (!lands) return `a hop landing as ${r.out}: no way to measure it`;
    if (lands.chain === "sol") return this.sol() ? null : "set SOLANA_RPC to see the hop land";
    return this.evm(lands.chainId) ? null : `set EVM_RPC_${lands.chainId} to see the hop land`;
  }

  /** The vault's balance of `t` at its own address there, now. */
  private async balanceNow(t: RemoteToken, address: string): Promise<bigint> {
    if (t.chain === "evm") return this.evm(t.chainId)!.balance(address, t.token);
    const conn = this.sol()!;
    if (t.mint === "SOL") return BigInt(await conn.getBalance(new PublicKey(address)));
    const accs = await conn.getParsedTokenAccountsByOwner(new PublicKey(address), { mint: new PublicKey(t.mint) });
    return accs.value.reduce((s, a) => s + BigInt(a.account.data.parsed.info.tokenAmount.amount), 0n);
  }

  /** Run one due remote node: build, step, keep, send, and report if it already landed. True if stepped. */
  async step(vault: string, vs: VaultFull, flow: number, node: number, n: RemoteNode, inbox: string, hidden?: { reveal: unknown; salt: string }): Promise<boolean> {
    const r = n.action.remote!;
    const tag = `${vault} flow ${flow} node ${node} "${n.action.label}"`;
    const amount = amountOf(n.input, n.when, inbox);
    const extra = hidden ? { reveal: hidden.reveal, salt: hidden.salt } : {};
    const base = { vault, flow, node, startedMs: Date.now() };
    let f: InFlight;
    let args: Record<string, unknown>;

    if (r.call.chain === "evm") {
      const cfg = vs.evm!;
      const p = evmPlan(cfg, r.call);
      if ("skip" in p) return void this.o.log(`${tag}: ${p.skip}`), false;
      const rpc = this.evm(cfg.chain_id)!;
      const me = cfg.address!;
      // The brain takes only its next nonce: the chain must be at it too, or the signed
      // transaction could never land (or would replace another).
      const onChain = Number(BigInt(await rpc.call<string>("eth_getTransactionCount", [me, "pending"])));
      if (onChain !== (cfg.next_nonce ?? 0)) return void this.o.log(`${tag}: the vault's next nonce is ${cfg.next_nonce ?? 0} on NEAR but ${onChain} on chain ${cfg.chain_id}: the owner must line them up; not stepped`), false;
      const block = await rpc.call<{ baseFeePerGas?: string }>("eth_getBlockByNumber", ["latest", false]);
      const baseFee = BigInt(block.baseFeePerGas ?? "0");
      if (baseFee > BigInt(cfg.max_fee)) return void this.o.log(`${tag}: base fee ${baseFee} wei is over the owner's cap ${cfg.max_fee}: waiting`), false;
      const need = BigInt(p.gas) * BigInt(cfg.max_fee) + (p.spendsEth ? amount : 0n);
      const has = await rpc.balance(me, "eth");
      if (has < need) return void this.o.log(`${tag}: the vault's address has ${has} wei, the transaction needs up to ${need} (gas at the owner's fee cap${p.spendsEth ? " + value" : ""}): not stepped`), false;
      let min = 0n;
      if (p.quote) {
        const quoter = this.o.env[`V4_QUOTER_${cfg.chain_id}`] ?? V4_QUOTERS[cfg.chain_id];
        if (!quoter) return void this.o.log(`${tag}: no Uniswap v4 quoter known on chain ${cfg.chain_id} (set V4_QUOTER_${cfg.chain_id})`), false;
        const out = await rpc.call<string>("eth_call", [{ to: quoter, data: v4QuoteCalldata(p.quote, amount) }, "latest"]).catch((e) => { this.o.log(`${tag}: quote failed: ${short(e)}`); return null; });
        if (!out) return false;
        const q = BigInt("0x" + out.slice(2, 66));
        min = lessSlippage(q, this.o.slippageBps);
        this.o.log(`${tag}: quote ${amount} in -> ${q} out; signing for at least ${min} (${this.o.slippageBps} bps under)`);
      }
      const tip = await rpc.call<string>("eth_maxPriorityFeePerGas", []).then(BigInt).catch(() => 0n);
      args = evmArgs(evmValues(p, min), cfg, tip, p.gas);
      f = { ...base, kind: "evm", chainId: cfg.chain_id, out: r.out ?? null, address: me, status: "stepping" };
    } else if (r.call.chain === "sol") {
      const cfg = vs.sol!;
      const p = solPlan(cfg, r.call, amount);
      if ("skip" in p) return void this.o.log(`${tag}: ${p.skip}`), false;
      const conn = this.sol()!;
      const me = cfg.address!;
      let message: string;
      try {
        if (p.kind === "pump_buy") {
          const b = await build.pumpBuy(conn, me, p.mint, p.lamports, this.o.solSlippagePct, true);
          if (BigInt(b.tokens) < p.floor) return void this.o.log(`${tag}: pump.fun gives ${b.tokens} coin units less ${this.o.solSlippagePct}%, under the owner's floor ${p.floor}: waiting`), false;
          message = b.message;
        } else if (p.kind === "pump_sell") {
          const b = await build.pumpSell(conn, me, p.mint, p.units, this.o.solSlippagePct);
          if (BigInt(b.minLamports) < p.floor) return void this.o.log(`${tag}: pump.fun pays ${b.lamports} lamports (${b.minLamports} with slippage), under the owner's floor ${p.floor}: waiting`), false;
          message = b.message;
        } else if (p.kind === "pay_sol") {
          message = (await build.paySol(conn, me, p.to, p.lamports)).message;
        } else {
          message = (await build.paySpl(conn, me, p.to, p.mint, p.units, p.decimals)).message;
        }
      } catch (e) {
        return void this.o.log(`${tag}: building the ${r.call.template} message failed: ${short(e)}`), false;
      }
      args = { chain: "sol", message };
      f = { ...base, kind: "sol", out: r.out ?? null, address: me, message, status: "stepping" };
    } else {
      // A hop: the brain builds the withdrawal to the vault's own address; we only watch it land.
      const lands = parseToken(r.out ?? "")!;
      const token = n.input.replace(/^intents:/, "");
      if (!hopChainOf(token)) return void this.o.log(`${tag}: ${token} is not a bridge token a hop serves`), false;
      const me = lands.chain === "sol" ? vs.sol?.address : vs.evm?.address;
      if (!me) return void this.o.log(`${tag}: the vault has no address on ${lands.chain === "sol" ? "Solana" : `chain ${lands.chainId}`} yet`), false;
      const before = await this.balanceNow(lands, me);
      args = { chain: "hop" };
      f = { ...base, kind: "hop", chainId: lands.chain === "evm" ? lands.chainId : undefined, out: r.out!, address: me, token, before: before.toString(), status: "stepping" };
    }

    // Kept before the step goes: a crash right after it still leaves a record to resume from.
    this.save(f);
    let logs: string[], nearTx: string;
    try {
      const res = await this.chain.vaultCallLogs(vault, "step_remote", { flow_id: flow, node, args, ...extra });
      ({ logs, hash: nearTx } = res);
    } catch (e) {
      this.save(f, true);
      this.o.log(`${tag}: step_remote refused: ${short(e)}`);
      return false;
    }
    const s = stepResult(logs, flow, node);
    if (s.failed || !s.step) {
      this.save(f, true);
      this.o.log(`${tag}: stepped but not signed (${s.failed?.why ?? "no remote_step"}): its input went back`);
      return true;
    }
    f = { ...f, min: s.step.min, max: s.step.max, nearTx };
    if (f.kind === "evm") {
      if (!s.evmRaw) return void this.o.log(`${tag}: no evm_signed in the outcome (tx ${nearTx}): the owner must settle it`), this.save({ ...f, status: "settle", note: "no signature seen" }), true;
      f = { ...f, raw: s.evmRaw, txHash: "0x" + Buffer.from(keccak_256(Buffer.from(s.evmRaw.slice(2), "hex"))).toString("hex"), status: "signed" };
    } else if (f.kind === "sol") {
      if (!s.solSignature) return void this.o.log(`${tag}: no sol_signed in the outcome (tx ${nearTx}): the owner must settle it`), this.save({ ...f, status: "settle", note: "no signature seen" }), true;
      f = { ...f, signature: s.solSignature, txHash: bs58.encode(Buffer.from(s.solSignature, "hex")), status: "signed" };
    } else {
      f = { ...f, status: "sent" };
    }
    this.save(f);
    this.o.log(`${tag}: stepped (${f.kind}, ${amount} in, report between ${f.min} and ${f.max}), NEAR tx ${nearTx}`);
    await this.advance(f, { flow, node, amount: amount.toString(), min_out: f.min!, max_out: f.max!, expires_ns: null, signed: true, hash: s.step.hash });
    return true;
  }

  /** Every in-flight step this keeper holds, and any pending on chain it doesn't: one pass. */
  async follow(vaults: Map<string, VaultFull | null>) {
    // Orphans are judged against what this keeper held when `vaults` was read: a step it reports
    // below is still in that (older) pending list, and is not an orphan.
    const held = { inflight: [...this.state.inflight] };
    for (const f of held.inflight) {
      const vs = vaults.has(f.vault) ? vaults.get(f.vault) : await this.chain.brainView<VaultFull | null>(this.brain, f.vault, "get_vault").catch(() => undefined);
      if (vs === undefined) continue; // can't see the brain right now: try next round
      const pending = vs?.remote?.find((p) => p.flow === f.flow && p.node === f.node) ?? null;
      await this.advance(f, pending).catch((e) => this.o.log(`${f.vault} flow ${f.flow} node ${f.node}: ${short(e)}`));
    }
    for (const [vault, vs] of vaults) {
      for (const p of orphans(vs?.remote ?? [], vault, held)) {
        this.ownerOnce(`${vault}#${p.flow}#${p.node}#orphan`, `${vault} flow ${p.flow} node ${p.node}: a remote step is pending (${p.hash}) that this keeper has no record of: the owner must settle it (remote_report, after looking on the other chain)`);
      }
    }
  }

  private ownerOnce(key: string, msg: string) {
    if (this.told.has(key)) return;
    this.told.add(key);
    this.o.log(msg);
  }

  /** Move one in-flight step on: broadcast it, see whether it landed, report. */
  private async advance(f: InFlight, pending: Pending | null) {
    const tag = `${f.vault} flow ${f.flow} node ${f.node}`;
    const next = nextFor(f, pending);
    if (next === "drop") {
      this.save(f, true);
      this.o.log(`${tag}: no longer pending on the brain (reported or settled): forgotten`);
      return;
    }
    if (next === "settle") {
      if (f.status !== "settle") this.save((f = { ...f, status: "settle", note: f.note ?? "signed, but this keeper never saw the signed bytes" }));
      this.ownerOnce(keyOf(f), `${tag}: ${f.note}: the owner must settle it (remote_report: what landed, or no output if it didn't)`);
      return;
    }
    if (pending) f = { ...f, min: f.min ?? pending.min_out, max: f.max ?? pending.max_out };
    if (next === "broadcast") {
      f = await this.broadcast(f);
      if (f.status !== "sent") return;
    }
    const landed = await this.landed(f);
    if (this.o.env.KEEPER_DEBUG) this.o.log(`${tag}: ${f.status} landed=${landed}`);
    if (landed === null) return;
    try {
      await this.chain.vaultCall(f.vault, "remote_report", { flow_id: f.flow, node: f.node, out: landed.toString() }, MAX_TGAS);
      this.save(f, true);
      this.o.log(`${tag}: reported ${landed} landed (${f.kind}${f.txHash ? ` ${f.txHash}` : ""})`);
    } catch (e) {
      const m = short(e);
      if (/no remote step pending/.test(m)) {
        this.save(f, true);
        this.o.log(`${tag}: already settled: forgotten`);
      } else {
        this.o.log(`${tag}: report of ${landed} refused: ${m}`);
      }
    }
  }

  private async broadcast(f: InFlight): Promise<InFlight> {
    const tag = `${f.vault} flow ${f.flow} node ${f.node}`;
    try {
      if (f.kind === "evm") {
        await this.evm(f.chainId!)!.call("eth_sendRawTransaction", [f.raw]).catch((e) => {
          // Sent before (a restart): it's the same transaction, its hash says where it is.
          if (!/already known|nonce too low|known transaction/i.test(short(e))) throw e;
        });
      } else {
        const wire = withSignature(Buffer.from(f.message!, "base64"), Buffer.from(f.signature!, "hex"));
        await this.sol()!.sendRawTransaction(wire, { skipPreflight: false }).catch((e) => {
          if (!/already been processed/i.test(short(e))) throw e;
        });
      }
    } catch (e) {
      const m = short(e);
      // Refused before landing (simulation failed, funds): it can't land as signed, and only the owner can call it failed.
      if (f.kind === "sol" || /revert|insufficient|underpriced|invalid/i.test(m)) {
        const g: InFlight = { ...f, status: "settle", note: `the chain refused the signed transaction (${m})` };
        this.save(g);
        this.ownerOnce(keyOf(g), `${tag}: ${g.note}: the owner must settle it (remote_report with no output)`);
        return g;
      }
      this.o.log(`${tag}: broadcast failed, will retry: ${m}`);
      return f;
    }
    const g: InFlight = { ...f, status: "sent" };
    this.save(g);
    this.o.log(`${tag}: broadcast ${f.txHash}`);
    return g;
  }

  /** What landed, once it did (null: not yet, or never: then the owner has been told). */
  private async landed(f: InFlight): Promise<bigint | null> {
    const tag = `${f.vault} flow ${f.flow} node ${f.node}`;
    const min = BigInt(f.min ?? "0"), max = BigInt(f.max ?? "0");
    const late = Date.now() - f.startedMs > (f.kind === "hop" ? this.o.hopWaitMs : this.o.txWaitMs);
    const out: Out = { token: f.out ? parseToken(f.out) : null };

    if (f.kind === "hop") {
      if (this.o.bridgeStatus && f.nearTx) {
        const st = withdrawalState(await bridge("withdrawal_status", { withdrawal_hash: f.nearTx }).catch(() => ({})), f.token!);
        if (st.status === "failed") {
          this.ownerOnce(keyOf(f), `${tag}: the bridge says the hop's withdrawal ${st.reason}: the owner must settle it`);
          return null;
        }
      }
      const delta = (await this.balanceNow(out.token!, f.address)) - BigInt(f.before!);
      const r = reportable(delta, min, max);
      if (r === null && late) this.ownerOnce(keyOf(f), `${tag}: the hop hasn't landed after ${Math.round((Date.now() - f.startedMs) / 60_000)} min (${delta} of at least ${min} seen at ${f.address}): still watching; the owner may settle it`);
      return r;
    }

    if (f.kind === "evm") {
      const rpc = this.evm(f.chainId!)!;
      const rc = await rpc.call<any>("eth_getTransactionReceipt", [f.txHash]);
      if (!rc) {
        if (this.o.env.KEEPER_DEBUG) this.o.log(`${tag}: ${f.txHash} not mined yet`);
        if (late) this.ownerOnce(keyOf(f), `${tag}: ${f.txHash} not mined after ${Math.round((Date.now() - f.startedMs) / 60_000)} min: still watching; the owner may settle it`);
        return null;
      }
      if (BigInt(rc.status) !== 1n) return this.cantLand(f, `${f.txHash} reverted`);
      let delta = 0n;
      if (out.token && out.token.chain === "evm") {
        const at = BigInt(rc.blockNumber);
        delta = (await rpc.balance(f.address, out.token.token, hex(at))) - (await rpc.balance(f.address, out.token.token, hex(at - 1n)));
        // ETH out: the transaction's own gas came out of the same balance.
        if (out.token.token === "eth") delta += BigInt(rc.gasUsed) * BigInt(rc.effectiveGasPrice);
      }
      const r = reportable(delta, min, max);
      return r === null ? this.cantLand(f, `measured ${delta} out, under the signed minimum ${min}`) : r;
    }

    // Solana: the transaction's own balances, before and after.
    const conn = this.sol()!;
    const tx = await conn.getTransaction(f.txHash!, { commitment: "confirmed", maxSupportedTransactionVersion: 0 });
    if (!tx) {
      // A Solana message expires with its blockhash (~1-2 min); resend it until then.
      if (Date.now() - f.startedMs < 90_000) {
        const wire = withSignature(Buffer.from(f.message!, "base64"), Buffer.from(f.signature!, "hex"));
        await conn.sendRawTransaction(wire, { skipPreflight: true }).catch(() => {});
      }
      if (late || Date.now() - f.startedMs > 180_000) this.ownerOnce(keyOf(f), `${tag}: ${f.txHash} not seen on Solana and its blockhash has expired: it can't land now; the owner must settle it (remote_report with no output)`);
      return null;
    }
    if (tx.meta?.err) return this.cantLand(f, `${f.txHash} failed on Solana (${JSON.stringify(tx.meta.err)})`);
    let delta = 0n;
    if (out.token && out.token.chain === "sol" && tx.meta) {
      const keys = tx.transaction.message.staticAccountKeys ?? (tx.transaction.message as any).accountKeys;
      if (out.token.mint === "SOL") {
        const i = keys.findIndex((k: any) => k.toBase58() === f.address);
        delta = BigInt(tx.meta.postBalances[i]) - BigInt(tx.meta.preBalances[i]) + (i === 0 ? BigInt(tx.meta.fee) : 0n);
      } else {
        // The vault's own token accounts of that mint: by owner, or (an RPC that leaves owner out) by
        // its associated account under either token program.
        const mint = out.token.mint;
        const atas = [TOKEN_PROGRAM, TOKEN_2022].map((p) => getAssociatedTokenAddressSync(new PublicKey(mint), new PublicKey(f.address), true, new PublicKey(p)).toBase58());
        const mine = (b: { owner?: string; accountIndex: number }) => (b.owner !== undefined ? b.owner === f.address : atas.includes(keys[b.accountIndex]?.toBase58()));
        const sum = (bs: typeof tx.meta.postTokenBalances) => (bs ?? []).filter((b) => b.mint === mint && mine(b)).reduce((s, b) => s + BigInt(b.uiTokenAmount.amount), 0n);
        delta = sum(tx.meta.postTokenBalances) - sum(tx.meta.preTokenBalances);
      }
    }
    const r = reportable(delta, min, max);
    return r === null ? this.cantLand(f, `measured ${delta} out, under the signed minimum ${min}`) : r;
  }

  private cantLand(f: InFlight, why: string): null {
    const g: InFlight = { ...f, status: "settle", note: why };
    this.save(g);
    this.ownerOnce(keyOf(g), `${f.vault} flow ${f.flow} node ${f.node}: ${why}: the keeper may not call it failed; the owner must settle it (remote_report with no output)`);
    return null;
  }
}

