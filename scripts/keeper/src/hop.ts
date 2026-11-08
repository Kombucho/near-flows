// Hops between chains through NEAR Intents' bridges: the keeper's side of the
// brain's hop.rs (out of the vault's Intents balance to its own address on
// another chain) and of the deposit templates (into the vault's Intents account).
// Pure helpers here, tested in hop.test.ts; the CLI is hop-main.ts.
//
// Formats, verified 2026-11-06:
// - Withdrawal (what the brain builds): intents.near ft_withdraw {token, receiver_id: token,
//   amount, memo: "WITHDRAW_TO:<address>"}, 1 yoctoNEAR (docs.near-intents.org, Verifier
//   contract > Withdrawals; near/intents contracts/poa/token/src/lib.rs WITHDRAW_MEMO_PREFIX;
//   defuse-protocol/sdk-monorepo intents-sdk poa-bridge-utils.ts createWithdrawIntentPrimitive).
//   On mainnet the EVM chains' omft tokens burn it (POA token); the Solana ones (omni-token
//   0.4.3) pass it to the bridge's withdraw relayer. Same call.
// - POA bridge JSON-RPC, POST https://bridge.chaindefuser.com/rpc (sdk-monorepo internal-utils
//   poaBridgeHttpClient): supported_tokens [{chains}], deposit_address [{account_id, chain}],
//   recent_deposits [{account_id, chain}], withdrawal_status [{withdrawal_hash: <NEAR tx hash>}].
//   Chains are "<blockchain>:<network>": sol:mainnet, eth:8453 (Base), eth:4663 (Robinhood).
//   One deposit address per Intents account and chain family (the same 0x on every EVM chain).
// - 1Click, https://1click.chaindefuser.com (openapi /docs/v0/openapi.yaml): GET /v0/tokens is
//   public; POST /v0/quote with dry: true prices a swap without making a deposit address (no
//   key needed; without a key 1Click adds its own 20 bps app fee to the quote); GET
//   /v0/status?depositAddress= tracks a live one.
//
// What Intents serves (1Click /v0/tokens and the bridge's supported_tokens, 2026-11-06):
// 216 tokens on ~40 chains, among them near, eth, base, arb, op, bsc, pol, avax, gnosis, bera,
// sol, btc, ton, tron, sui, aptos, xrp, zec, monad, plasma, scroll, starknet, stellar, hood.
// - Solana (sol:mainnet, POA): SOL sol.omft.near (9 dec, min deposit 0.00125 SOL, withdrawal fee
//   0.000014 SOL), USDC sol-5ce3bf3a31af18be40ba30f721101b4341690186.omft.near (6 dec, fee 0.03),
//   USDT, and memecoins (WIF, BOME, TRUMP...).
// - Base (eth:8453, POA): ETH base.omft.near (18 dec, fee 0.00000056 ETH), USDC
//   base-0x833589fcd6edb6e08f4c7c32d4f71b54bda02913.omft.near (fee 0.0024), WETH, cbBTC...
// - Robinhood Chain IS supported (blockchain "hood", eth:4663, POA): ETH hood.omft.near (fee
//   0.00001 ETH), WETH hood-0x0bd7d308f8e1639fab988df18a8011f41eacad73.omft.near, USDG
//   hood-0x5fc5360d0400a0fd4f2af552add042d716f1d168.omft.near (6 dec, fee 0.075), USDe, and
//   two memecoins. No tokenized stocks (NVDA etc.): those stay on Robinhood Chain.

export const BRIDGE_RPC = "https://bridge.chaindefuser.com/rpc";
export const ONECLICK = "https://1click.chaindefuser.com";
export const WITHDRAW_MEMO_PREFIX = "WITHDRAW_TO:";

/** EVM chains the POA bridge serves with omft tokens: the token's label and its chain id (as the brain's hop.rs). */
export const EVM_LABELS: Record<string, number> = { eth: 1, base: 8453, arb: 42161, gnosis: 100, bera: 80094, hood: 4663 };

export type HopChain = { kind: "sol" } | { kind: "evm"; chainId: number };

/** Where a POA token (`sol.omft.near`, `base-0x….omft.near`) lands, or null for anything else. Mirrors the brain. */
export function hopChainOf(token: string): HopChain | null {
  const dot = token.indexOf(".");
  if (dot < 1) return null;
  const parent = token.slice(dot + 1);
  if (!(parent === "omft.near" || parent.startsWith("omft."))) return null;
  const label = token.slice(0, dot).split("-")[0];
  if (label === "sol") return { kind: "sol" };
  return label in EVM_LABELS ? { kind: "evm", chainId: EVM_LABELS[label] } : null;
}

/** The bridge API's chain name for a token's landing chain. */
export function bridgeChain(c: HopChain): string {
  return c.kind === "sol" ? "sol:mainnet" : `eth:${c.chainId}`;
}

/** The token-id prefix a hop node's reported output must have (as the brain checks). */
export function landsAs(c: HopChain): string {
  return c.kind === "sol" ? "sol:" : `evm:${c.chainId}:`;
}

export const withdrawMemo = (address: string) => `${WITHDRAW_MEMO_PREFIX}${address}`;

/** The ft_withdraw arguments the brain builds for a hop: for checking a vault's emitted hop event. */
export function withdrawArgs(token: string, amount: bigint, address: string) {
  return { token, receiver_id: token, amount: amount.toString(), memo: withdrawMemo(address) };
}

/** What a hop node should report once the bridge paid out: the amount less the bridge's fee, and
 * whether the brain will take it (between amount - maxFee and amount). */
export function hopReport(amount: bigint, bridgeFee: bigint, maxFee: bigint): { out: bigint; accepted: boolean } {
  const out = amount > bridgeFee ? amount - bridgeFee : 0n;
  return { out, accepted: out <= amount && out + maxFee >= amount };
}

export function rpcBody(method: string, params: Record<string, unknown>) {
  return { jsonrpc: "2.0", id: 1, method, params: [params] };
}

export type QuoteArgs = { from: string; to: string; amount: bigint; refundTo: string; recipient: string;
  recipientType?: "INTENTS" | "DESTINATION_CHAIN"; depositType?: "INTENTS" | "ORIGIN_CHAIN"; slippageBps?: number; minutes?: number; now?: Date };

/** An Intents asset id: `nep141:` unless it already has a prefix (`nep141:`, `1cs_v1:`...). */
export const asset = (s: string) => (s.includes(":") ? s : `nep141:${s}`);

/** A 1Click quote request, always a dry run: prices only, no deposit address, nothing to fund. */
export function quoteBody(a: QuoteArgs) {
  const now = a.now ?? new Date();
  return {
    dry: true,
    swapType: "EXACT_INPUT",
    slippageTolerance: a.slippageBps ?? 100,
    originAsset: asset(a.from),
    depositType: a.depositType ?? "INTENTS",
    destinationAsset: asset(a.to),
    amount: a.amount.toString(),
    refundTo: a.refundTo,
    refundType: a.depositType === "ORIGIN_CHAIN" ? "ORIGIN_CHAIN" : "INTENTS",
    recipient: a.recipient,
    recipientType: a.recipientType ?? "INTENTS",
    deadline: new Date(now.getTime() + (a.minutes ?? 30) * 60_000).toISOString().replace(/\.\d{3}Z$/, "Z"),
  };
}

export type WithdrawalState = { status: "pending" } | { status: "completed"; txHash: string | null; amount?: number } | { status: "failed"; reason: string };

/** The bridge's withdrawal_status answer for one token (the list is unsorted: matched by token). */
export function withdrawalState(resp: any, nearToken: string): WithdrawalState {
  if (resp?.error) return String(resp.error).includes("not found") ? { status: "pending" } : { status: "failed", reason: String(resp.error) };
  const w = (resp?.result?.withdrawals ?? []).find((x: any) => x?.data?.near_token_id === nearToken || x?.data?.defuse_asset_identifier?.endsWith?.(nearToken));
  if (!w || w.status === "PENDING") return { status: "pending" };
  if (w.status === "COMPLETED") return { status: "completed", txHash: w.data?.transfer_tx_hash ?? null, amount: w.data?.amount };
  return { status: "failed", reason: String(w.status) };
}

/** One token's bridge terms from supported_tokens: decimals, minimum deposit, withdrawal fee. */
export function tokenTerms(resp: any, nearToken: string): { decimals: number; minDeposit: bigint; withdrawalFee: bigint } | null {
  const t = (resp?.result?.tokens ?? []).find((x: any) => x.near_token_id === nearToken);
  return t ? { decimals: t.decimals, minDeposit: BigInt(t.min_deposit_amount), withdrawalFee: BigInt(t.withdrawal_fee) } : null;
}

export async function bridge(method: string, params: Record<string, unknown>): Promise<any> {
  const r = await fetch(BRIDGE_RPC, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(rpcBody(method, params)) });
  return r.json();
}

export async function oneClick(path: string, body?: unknown): Promise<any> {
  const r = await fetch(`${ONECLICK}${path}`, body === undefined ? {} : { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  const text = await r.text();
  try { return JSON.parse(text); } catch { return { status: r.status, text }; }
}
