// The chat server's only window onto the chain: read-only RPC views.

import { JsonRpcProvider } from "near-api-js";
import type { KeyChecker } from "./auth.ts";
import type { Membership } from "./rooms.ts";

export class Chain implements KeyChecker, Membership {
  private rpc: JsonRpcProvider;

  constructor(rpcUrl: string, private bazaar: string) {
    this.rpc = new JsonRpcProvider({ url: rpcUrl });
  }

  async isFullAccessKey(accountId: string, publicKey: string): Promise<boolean> {
    try {
      const k = await this.rpc.viewAccessKey({ accountId, publicKey });
      return k.permission === "FullAccess";
    } catch {
      return false; // no such account or key
    }
  }

  async lotExists(lotId: number): Promise<boolean> {
    const lot = await this.rpc.callFunction({ contractId: this.bazaar, method: "get_lot", args: { id: lotId } });
    return lot != null;
  }

  async isParticipant(lotId: number, accountId: string): Promise<boolean> {
    const ok = await this.rpc.callFunction({ contractId: this.bazaar, method: "is_participant", args: { lot_id: lotId, account_id: accountId } });
    return ok === true;
  }
}
