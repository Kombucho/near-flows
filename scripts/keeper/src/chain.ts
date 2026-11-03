// Talking to NEAR: read-only views and the few transactions the keeper sends.
//
// Keys: the keeper's private key comes from the KEEPER_PRIVATE_KEY environment
// variable, or from the file near-cli saves (~/.near-credentials/<network>/<account>.json).
// It is never printed or written anywhere. Best practice: give the keeper a
// key that can only call this market and the two token contracts.

import { Account, JsonRpcProvider, KeyPairSigner, type KeyPairString } from "near-api-js";
import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const TGAS = 10n ** 12n;
const ONE_YOCTO = 1n; // NEP-141 transfers require exactly 1 yoctoNEAR attached

export type MarketConfig = {
  token: string;
  quote: string;
  native: boolean;
  launched: boolean;
  paused: boolean;
  price: string; // U128: quote per token, 1e18 fixed-point
  current_fee_bps: number;
};

export function loadKey(network: string, accountId: string): KeyPairString {
  const fromEnv = process.env.KEEPER_PRIVATE_KEY;
  if (fromEnv) return fromEnv as KeyPairString;
  const file = join(homedir(), ".near-credentials", network, `${accountId}.json`);
  if (existsSync(file)) return JSON.parse(readFileSync(file, "utf8")).private_key as KeyPairString;
  throw new Error(`no key for ${accountId}: set KEEPER_PRIVATE_KEY or save the key with near-cli (legacy keychain) to ${file}`);
}

export class Chain {
  readonly provider: JsonRpcProvider;
  private account?: Account;

  constructor(rpcUrl: string, readonly market: string, keeperId?: string, key?: KeyPairString) {
    this.provider = new JsonRpcProvider({ url: rpcUrl });
    if (keeperId && key) this.account = new Account(keeperId, this.provider, KeyPairSigner.fromSecretKey(key));
  }

  async view<T>(contractId: string, method: string, args: Record<string, unknown> = {}): Promise<T> {
    return (await this.provider.callFunction({ contractId, method, args })) as T;
  }

  config(): Promise<MarketConfig> {
    return this.view<MarketConfig>(this.market, "get_config");
  }

  async balance(token: string, who: string): Promise<bigint> {
    return BigInt(await this.view<string>(token, "ft_balance_of", { account_id: who }));
  }

  async decimals(token: string): Promise<number> {
    return (await this.view<{ decimals: number }>(token, "ft_metadata")).decimals;
  }

  private signer(): Account {
    if (!this.account) throw new Error("this action needs the keeper's key (run with --send)");
    return this.account;
  }

  get keeperId(): string {
    return this.signer().accountId;
  }

  /** Positions with earnings waiting to be paid to their owners. */
  settleable(limit = 20): Promise<number[]> {
    return this.view<number[]>(this.market, "get_settleable", { limit });
  }

  /** Pay a position's earnings to its OWNER (anyone may call; nothing comes to us). */
  settle(positionId: number) {
    return this.signer().callFunction({ contractId: this.market, methodName: "settle", args: { position_id: positionId }, gas: 100n * TGAS });
  }

  crank() {
    return this.signer().callFunction({ contractId: this.market, methodName: "crank", args: {}, gas: 100n * TGAS });
  }

  /** Send `amount` of `token` into the market with a JSON message. */
  private transferCall(token: string, amount: bigint, msg: object) {
    return this.signer().callFunction({
      contractId: token,
      methodName: "ft_transfer_call",
      args: { receiver_id: this.market, amount: amount.toString(), msg: JSON.stringify(msg) },
      deposit: ONE_YOCTO,
      gas: 150n * TGAS,
    });
  }

  buy(quoteToken: string, quoteIn: bigint, minOut: bigint) {
    return this.transferCall(quoteToken, quoteIn, { action: "buy", min_out: minOut.toString() });
  }

  sell(token: string, tokenIn: bigint, minOut: bigint) {
    return this.transferCall(token, tokenIn, { action: "sell", min_out: minOut.toString() });
  }

  /**
   * LIVE mode only: the price of `token` in `quote` on a Rhea/Ref classic pool,
   * as quote-per-token 1e18 fixed-point. Experimental: not exercised on testnet.
   */
  async refPrice(refContract: string, poolId: number, token: string, quote: string): Promise<bigint> {
    const pool = await this.view<{ token_account_ids: string[]; amounts: string[] }>(refContract, "get_pool", { pool_id: poolId });
    const iToken = pool.token_account_ids.indexOf(token);
    const iQuote = pool.token_account_ids.indexOf(quote);
    if (iToken < 0 || iQuote < 0) throw new Error(`pool ${poolId} does not pair ${token} with ${quote}`);
    return (BigInt(pool.amounts[iQuote]) * 10n ** 18n) / BigInt(pool.amounts[iToken]);
  }
}
