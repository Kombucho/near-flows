import { SOURCE_COMMIT } from "./limits";

/** Facts read from origin/main after the merge. Not a chain query. */
export const ABI = {
  commit: SOURCE_COMMIT,
  commitDate: "2026-11-08T11:47:09+01:00",
  verified: true,
  deployment: {
    source: "README.md Live on testnet",
    suffix: "fl06",
    brain: "nf-brain-fl06.testnet",
    factory: "nf-relay-fl06.testnet",
    relayCodeHash: "CsgeCTj9dVdTnWnHtTJpDa3dMYuejdy7WsM42smmwov2",
    board: "nf-board-fl06.testnet",
    demoVault: "demo.nf-relay-fl06.testnet",
    caveat: "README says that deployed price board predates the two-publisher check and needs a redeploy. These ids were not re-queried.",
  },
  notes: [
    {
      topic: "Standalone vault",
      status: "verified",
      detail: "contracts/vault and contracts/factory are deleted. Only the relay factory remains.",
    },
    {
      topic: "Create",
      status: "verified",
      detail: "create_vault(name, keeper, expected_code_hash?) and create_stealth_vault(name, owner_key, keeper, expected_code_hash?). Attach at least 0.1 NEAR. A second named vault for the same signer is refused: you already have a vault. A closed name stays closed (is_closed). expected_code_hash pins the published relay.",
    },
    {
      topic: "Several vaults",
      status: "verified-limit",
      detail: "One named vault per signer. Stealth vaults are extra and the payer is not stored as owner. Several rows in this app are local drafts, not extra chain vaults.",
    },
    {
      topic: "Direct signing",
      status: "verified",
      detail: "evm_call, sol_sign_swap, sol_sign_tx, and intents_sign_swap are owner only in brain request(). A keeper reaches other chains through step_remote.",
    },
    {
      topic: "Close",
      status: "verified",
      detail: "Owner may close. The keeper may close only after auto-close is on, and only to the owner's beneficiary. Flows must be empty, no step in flight, token and Intents balances read as zero. Other chains or unmeasured positions need the owner and confirm_remote. The factory tombstones the name.",
    },
    {
      topic: "Fees",
      status: "verified",
      detail: "A fee increase waits 24 hours (FEE_DELAY_NS) and shows as pending. A cut applies at once. The signature-fee cap cannot be set above 0.1 NEAR (MAX_SIGNATURE_FEE_CAP). This screen does not read get_fees, so the number in force is not shown.",
    },
    {
      topic: "Oracle",
      status: "verified",
      detail: "A feed's backup is another publisher (Backup::Other) or single_source. A feed cannot back itself. Market floors refuse single_source. The EMA-as-backup option is gone.",
    },
    {
      topic: "Visibility",
      status: "verified-readonly",
      detail: "hide_flow and save_hidden_flow exist and are owner only. This app does not call them. Private here is not chain secrecy.",
    },
    {
      topic: "Intents hop",
      status: "verified-not-a-recipe",
      detail: "intents_sign_swap is still a one-shot owner call, not a flow node. The hop stays in the local draft and blocks contract serialization by name.",
    },
    {
      topic: "Demo log",
      status: "absent",
      detail: "No demo.log in the tree. scripts/demo/run.sh was not run. Import stays empty until a DEMO| file is chosen.",
    },
  ],
} as const;
