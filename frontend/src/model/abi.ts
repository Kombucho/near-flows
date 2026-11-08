import { SOURCE_COMMIT } from "./limits";

/** Facts checked against origin/main. User-reported revisions are listed only as absent. */
export const ABI = {
  commit: SOURCE_COMMIT,
  commitDate: "2026-11-06T16:43:20+01:00",
  resyncedAt: "2026-11-08",
  userReportedChangesInTree: false,
  notes: [
    {
      topic: "Standalone vault",
      onThisCommit: "contracts/vault and contracts/factory are still in the tree. create_vault still exists on both factories.",
      userReported: "Standalone vault deleted. No separate adapter is planned.",
      ui: "The app does not call the standalone vault. It is listed as still present, not as a second integration.",
    },
    {
      topic: "Vaults per login",
      onThisCommit: "factory_relay.create_vault refuses with \"you already have a vault\" for the same predecessor. create_stealth_vault is a separate key-owned account and does not use that map.",
      userReported: "Several vaults under one login are already supported. Do not enforce one vault per login.",
      ui: "Local drafts can list many vaults. Plus does not send. The on-chain one-signer limit is shown as a mismatch, not as a disabled Plus.",
    },
    {
      topic: "Direct signing",
      onThisCommit: "brain evm.rs, sol.rs and intents.rs comments say owner or keeper may call evm_call, sol_sign_swap, sol_sign_tx and intents_sign_swap.",
      userReported: "Those direct calls are owner-only. A keeper reaches other chains only through flow steps.",
      ui: "Palette has no keeper button for those methods. They stay in a disarmed owner review. step and step_remote remain the flow-step names.",
    },
    {
      topic: "Close",
      onThisCommit: "Req::Close exists. Auto-close lets anyone close an empty vault. is_closed and closed-name tombstones exist on the relay factory. Holdings checks in the user report were not re-derived as a new predicate.",
      userReported: "Owner or keeper may close, with holdings checks. Closed names are permanent.",
      ui: "Close stays a disarmed settings action. is_closed is described, not queried.",
    },
    {
      topic: "Fees",
      onThisCommit: "Protocol fee is set_fee by the fee admin inside a deploy-time cap. Keeper fee is set with the keeper, max 0.05 NEAR, no 24h delay. Spending-limit loosening waits 24h. That delay is not a protocol-fee delay.",
      userReported: "Fee increases wait 24 hours. Signature fee is capped.",
      ui: "No fee numbers are shown as current. Pending-increase time is Unavailable.",
    },
    {
      topic: "Oracle",
      onThisCommit: "A feed has a spot read plus an optional backup: EMA on the same contract, or another contract. That is the spot-plus-backup model.",
      userReported: "Two independent sources replace spot plus average.",
      ui: "Oracle setup is not offered as a live form. The backup enum is named as what this commit contains.",
    },
    {
      topic: "Create arguments",
      onThisCommit: "create_vault(name, keeper) and create_stealth_vault(name, owner_key, keeper). Minimum attached deposit in this commit is 0.1 NEAR.",
      userReported: "New arguments, errors, and a redeployed testnet.",
      ui: "Create review does not serialize a payload. No factory id is filled in.",
    },
    {
      topic: "Intents hop",
      onThisCommit: "intents_sign_swap is a one-shot request, not a flow Recipe. No library recipe routes through Intents.",
      userReported: "Show an Intents hop. Map it to the real method.",
      ui: "The hop is a visible non-serialized node. It is not wired to intents_sign_swap and not offered to a keeper.",
    },
    {
      topic: "Demo log",
      onThisCommit: "scripts/demo/run.sh prints DEMO| lines from contracts/sandbox/tests/demo.rs. contracts/target/demo.log was not in the tree. The script was not run.",
      userReported: "Replay a genuine four-leg fork run.",
      ui: "Demo opens on an empty recorded-replay state. An imported DEMO| log can be played. LOCAL FORK LIVE has no feed.",
    },
  ],
} as const;
