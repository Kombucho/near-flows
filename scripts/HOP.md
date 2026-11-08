# Hops: moving a vault's money between chains through NEAR Intents

A relay vault already has its own account on NEAR Intents (`intents_setup`) and its own
addresses on Solana and EVM chains (`sol_setup`, `evm_setup`, MPC-derived from the vault's
account). A **hop** moves money between them through the Intents bridges, and only between
them: no hop can pay anyone else.

- **Out** (Intents to another chain): node `intents_hop_out` in `nodes/library.json`, brain
  `contracts/brain/src/hop.rs`. The brain builds the withdrawal itself:
  `intents.near ft_withdraw {token, receiver_id: token, amount, memo: "WITHDRAW_TO:<the vault's own address>"}`.
  The keeper never names a destination. Capped per hop (`intents_hop`), counted on the
  `intents:<token>` spending limit, and as a flow node its output is reported by the keeper
  between `amount - max_fee` and `amount`.
- **In** (another chain to Intents): node `intents_deposit`, templates `intents_deposit_*` in
  `nodes/sol-templates.json` and `nodes/evm-templates.json` (`intents_deposits`). The owner pins
  the vault's Intents deposit address once; the keeper only fills the amount.
- **Keeper side**: `scripts/keeper/src/hop.ts` and `npm run hop` (read-only lookups, dry-run
  quotes, withdrawal status). It never signs or sends anything.

## The formats, and where they come from (checked 2026-11-06)

| what | format | source |
|---|---|---|
| withdraw a bridged token | `ft_withdraw {token, receiver_id: <the token itself>, amount, memo: "WITHDRAW_TO:<address>"}`, 1 yoctoNEAR, no `msg` (then the verifier refunds if the token refuses) | docs.near-intents.org > Verifier contract > Withdrawals; near/intents `contracts/poa/token/src/lib.rs` (`WITHDRAW_MEMO_PREFIX`); defuse-protocol/sdk-monorepo `intents-sdk/src/bridges/poa-bridge/poa-bridge-utils.ts` |
| what the token does with it | EVM chains' omft tokens (`base.omft.near`, `hood.omft.near`...): the POA token, burns. Solana's (`sol.omft.near`, Solana USDC): Omni Bridge's omni-token 0.4.3, hands it, memo and all, to the bridge's withdraw relayer | mainnet `view_code` / `view_state` of the token accounts; Near-One/omni-bridge `near/omni-token/src/lib.rs` |
| bridge API | `POST https://bridge.chaindefuser.com/rpc`: `supported_tokens [{chains}]`, `deposit_address [{account_id, chain}]`, `recent_deposits [{account_id, chain}]`, `withdrawal_status [{withdrawal_hash}]` | sdk-monorepo `internal-utils/src/poaBridge/poaBridgeHttpClient` |
| chain names | `sol:mainnet`, `eth:8453` (Base), `eth:4663` (Robinhood Chain), `eth:1`, `eth:42161`... | same |
| 1Click quotes | `POST https://1click.chaindefuser.com/v0/quote` with `dry: true`: a price, no deposit address, nothing to fund; no key needed (without one 1Click adds a 20 bps app fee to its quote) | `1click.chaindefuser.com/docs/v0/openapi.yaml` |

## What Intents serves (1Click `/v0/tokens` and the bridge, 2026-11-06)

216 tokens on about 40 chains. For the demo:

| chain | token on Intents | decimals | bridge withdrawal fee | min deposit |
|---|---|---|---|---|
| Solana | SOL `sol.omft.near` | 9 | 0.000014 SOL | 0.00125 SOL |
| Solana | USDC `sol-5ce3bf3a31af18be40ba30f721101b4341690186.omft.near` | 6 | 0.03 USDC | 0.001 USDC |
| Base | ETH `base.omft.near` | 18 | 0.00000056 ETH | 0.0000001 ETH |
| Base | USDC `base-0x833589fcd6edb6e08f4c7c32d4f71b54bda02913.omft.near` | 6 | 0.0024 USDC | 0.15 USDC |
| Robinhood Chain | ETH `hood.omft.near` | 18 | 0.00001 ETH | - |
| Robinhood Chain | USDG `hood-0x5fc5360d0400a0fd4f2af552add042d716f1d168.omft.near` | 6 | 0.075 USDG | - |

**Robinhood Chain is supported** (blockchain `hood`, `eth:4663`): ETH, WETH, USDG, USDe and two
memecoins. Its tokenized stocks (NVDA...) are not: they stay on Robinhood Chain, bought and sold
there by the vault's templates. One deposit address serves every EVM chain.

Look it up again any time: `npm run hop -- tokens sol base hood`, `npm run hop -- terms eth:4663`.

## A mainnet hop, step by step (not run: this is the plan)

A round trip NEAR -> Solana -> NEAR with ~$10. Every step below is a separate, explicit OK
from the owner (AGENTS.md rule 1); nothing in the repo sends to mainnet on its own.

**Accounts.** The owner's mainnet NEAR account; a keeper account (can be the owner, fee 0);
a vault on a mainnet brain. There is no hosted mainnet brain yet, so either the maintainers
deploy one, or the owner self-hosts it (path (b) of AGENTS.md, pointed at mainnet).

1. **Vault and addresses.** `create_vault` (0.1 NEAR float). Then, through `x`:
   `sol_setup {signer: "v1.signer", max_cu_price, max_cu_limit}`,
   `evm_setup {signer: "v1.signer", chain_id: 8453, ...}` (the same address on every EVM chain),
   `intents_setup {verifier: "intents.near", signer: "v1.signer"}` (registers the vault's MPC key
   on its Intents account). The brain stores the Solana and EVM addresses.
2. **Into Intents.** Send ~2.7 NEAR (~$10) to the vault, wrap it, and deposit the wNEAR into its
   Intents account (`withdraw_to {via_intents: true}` to itself, or a one-call recipe
   `ft_transfer_call` to `intents.near` with `msg: ""`).
3. **Swap inside Intents.** `intents_allow` wNEAR -> `sol.omft.near` with a floor (or a market
   floor from the price board), then `intents_sign_swap`. Before signing, price it for free:
   `npm run hop -- quote wrap.near sol.omft.near 2700000000000000000000000 <vault>`.
   Gap: on mainnet a solver must take the other side. The keeper's `intents-solver.ts` is a
   testnet self-solver; the mainnet path is the Solver Bus (`quote`, then `publish_intent` with the
   vault's signed intent), not built yet.
4. **Hop out to Solana.** Owner: `intents_hop {hop: {token: "sol.omft.near", chain: "sol",
   max: "50000000", max_fee: "100000"}}` (at most 0.05 SOL per hop, fee bound 0.0001 SOL) and
   `set_limit {token: "intents:sol.omft.near", ...}`. Then either a flow node
   `{input: "intents:sol.omft.near", action: {calls: [], out: null, remote: {call: {chain: "hop"}, out: "sol:SOL"}}}`
   run with `step_remote {args: {chain: "hop"}}`, or a one-off `intents_hop_out {token, amount}`.
   Follow it: `npm run hop -- status <near tx hash> sol.omft.near --wait`, then (flow node)
   `remote_report {out: <amount - 14000>}`.
5. **Use it on Solana.** The SOL is at the vault's own Solana address: pump.fun, Meteora, Hylo,
   Jupiter nodes, as on the forks.
6. **Hop back in.** `npm run hop -- deposit-address <vault> sol:mainnet`; the owner pins it:
   `sol_template` `intents_deposit_sol {deposit_address, max_lamports}`. The keeper builds a
   System transfer (`SOLANA_RPC=<mainnet rpc> npx tsx src/fork-sol.ts pay-sol <vault sol> <deposit> <lamports>`),
   `sol_sign_tx`, broadcasts (`fork-sol.ts send`), and checks `npm run hop -- deposits <vault> sol:mainnet`.
   USDC needs the deposit address's USDC account to exist first (a fresh deposit address has none;
   anyone can create it, ~0.002 SOL rent, not from the vault: its templates can't pay others' rent).
7. **Out of Intents.** Swap back to wNEAR (step 3 reversed) and `intents_withdraw` (always to the vault).

### What it costs

| item | about | refundable? |
|---|---|---|
| mainnet brain + relay factory + relay publish (only if self-hosted; skip with a hosted brain) | brain ~8.6 NEAR staked, factory ~1.7 staked, relay publish ~1.0 NEAR | stakes yes (delete the accounts), publish no |
| vault | 0.108 NEAR (mostly its own float) | yes, auto-close leaves ~0.01 NEAR spent |
| NEAR gas | a hop burns ~15-21 Tgas (sandbox), a signed Solana/EVM action ~20-25 Tgas: ~0.002 NEAR each at 0.1 NEAR/1000 Tgas | no |
| MPC signatures | `v1.signer` deposit per `sign` (the brain attaches 1 yoctoNEAR; check the signer's current deposit before running), plus the brain's own signature fee if its fees are on (0.01 NEAR; a self-hosted brain can run with none) | no |
| Intents swap | the solver's spread, ~0.2% on a $10 wNEAR -> SOL quote today; the verifier's fee 0.0001% | no |
| bridge out | SOL 0.000014 SOL (~$0.001), Solana USDC 0.03, Base ETH ~$0.001, Robinhood ETH 0.00001 ETH (~$0.02), USDG 0.075 | no |
| bridge in | no fee listed; minimum deposits above | - |
| Solana / Base fees | 5,000 lamports a signature (~$0.0005); Base ~$0.001-0.01 a transfer | no |

A $10 round trip NEAR -> Solana -> NEAR on a hosted brain: about $0.05 in gas and fees plus the
two swap spreads (~$0.04), well under a dollar. Self-hosting the brain adds ~1 NEAR burned
once (the relay publish) and ~10 NEAR staked and refundable.

## Tests

- `contracts/sandbox/tests/hop.rs`: the outbound hop against the real intents.near verifier and
  both mainnet token codes (fetch them with `scripts/fetch-protocols.sh`).
- `contracts/sandbox/tests/hop_fork.rs`: deposits into the pinned address, executed on a Base fork
  (anvil, port 9900) and a Solana fork (surfpool, ports 9902/9903), and every other recipient refused.
- `scripts/keeper/src/hop.test.ts`: the keeper's pure parts (`npm test`).
