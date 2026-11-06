# Relay deploy (testnet)

`scripts/deploy-relay-testnet.sh` deploys the split design: brain, relay factory, relay code published as a global contract, price board, and one demo vault with oracle feeds and a Rhea flow.

```
SUFFIX=abcde DRY=1 ./scripts/deploy-relay-testnet.sh   # prints every command, sends nothing
SUFFIX=abcde STEP=costs ./scripts/deploy-relay-testnet.sh
SUFFIX=abcde ./scripts/deploy-relay-testnet.sh         # the real run (owner approves first)
```

Stages, in order: `build accounts brain factory publish priceboard vault flow check`. You can run one at a time (`STEP=vault`) or a comma list. Before a stage spends anything, it reads the paying account's balance and stops if it's short. The message it prints includes the exact `send-near` command to top the account up.

## What it costs

Storage costs 0.01 NEAR per KB, and a global contract costs 10 times that. Each account starts with 10 NEAR from the faucet. The "measured" column comes from `contracts/sandbox/tests/deploy_sequence.rs`, which replays the script.

| stage | paid by | estimate | measured | what | refundable? |
|---|---|---|---|---|---|
| brain | nf-brain | 8.66 | 8.64 | 860,722 B of code (8.607 locked) + gas | yes: delete the account |
| factory | nf-relay | 1.68 | 1.66 | 165,011 B of code (1.650 locked) + gas | yes: delete the account |
| publish | nf-relay | 1.05 | 1.00 | 9,989 B of relay global code (0.9989, **burned**) | **no** |
| priceboard | nf-board | 2.56 | 2.53 | 252,909 B of code (2.529 locked) + gas | yes: delete the account |
| vault + flow | nf-owner | 6.39 | 6.32 | 1 into the vault + 5 sent to it + token storage 0.0375 + Rhea 0.1 + gas | see below |
| (later) | nf-keeper | 0.1 | 0 | gas for steps and Pyth pushes | - |

The total is about 20.4 NEAR across 5 faucet accounts. Every account fits inside its own 10 NEAR, so no pooling is needed. The brain is the tightest: about 1.39 NEAR is left after the deploy. It doesn't need more, because each vault pays the brain for its own record (the demo vault paid 0.033 NEAR for a 0.028 NEAR record). If the faucet gives less than 10 NEAR, run with `FAUCET=<n>` and the table will flag what needs topping up.

## Refunds and rollback

- **Brain, factory, board**: `near account delete-account <acc> beneficiary <your account> network-config testnet sign-with-legacy-keychain send` returns the whole balance, storage included. Deleting the brain also strands every vault. The vault records the vaults paid for go to the beneficiary, not back to the vault owners.
- **Relay code**: the ~1 NEAR global deploy is burned. Nothing gets it back, and re-publishing costs it again.
- **Vault**: keyless by design, so it can never be deleted. The owner gets its NEAR back with `x {"drain": ...}` / `x {"withdraw": {"token": "near", "amount": ...}}`. The 0.05 NEAR reserve, its own storage (193 B) and token storage deposits stay with it.
- **Token storage deposits**: these belong to the vault on each token contract. Recovering them needs `storage_unregister`, which the brain never lets a vault call. Treat them as spent (0.04 NEAR plus Rhea's 0.1).
- **Order for a full rollback**: withdraw from the vault first (it needs the brain), then delete the board, factory and brain.

## Before demoing market floors or price triggers

The script stores the Pyth stable price ids. `pyth-oracle.testnet` accepts stable updates (its data source is Pythnet's stable emitter), but on 2026-11-06 none of the five feeds existed there yet. The `price_feed_exists` check returned false, and only stale beta-channel feeds had prices. Until the keeper pushes them, every floor or trigger correctly refuses with "no price fresher than 60 s":

```
cd scripts/keeper && PYTH_API_KEY=... KEEPER_ACCOUNT_ID=nf-keeper-$SUFFIX.testnet npm run pyth -- --vault demo.nf-relay-$SUFFIX.testnet
```

`STEP=check` shows whether each feed exists and how old its price is. The demo flow has no market floors on purpose: testnet Rhea pools don't track real prices (1 wNEAR buys about 1.24 testnet USDT), so a floor would refuse every swap.

## Notes

- The relay is published by calling the factory's `publish` (`call-function ... file-args relay.wasm`), not with `near contract deploy-as-global`. That way the factory records the hash its vaults are created from. The global deploy is charged to the factory's balance. The attached 1.099 NEAR (the wasm size × 0.1 NEAR/KB, plus 0.1) is the factory paying itself, so it can be sure it holds that amount.
- The brain keeps its full-access key on testnet so it can be deleted for the refund. On mainnet, delete its keys after deploy so it becomes immutable, as relay/src/lib.rs assumes.
- The price board starts attested by the keeper's own key with k=1, which means trusting the keeper. Add attesters with `set_attesters` (the owner is nf-owner).
- Fees: the brain's init now takes `fees` (brain/src/fees.rs): treasury and fee admin (both nf-owner here, `TREASURY_ID` overrides), protocol fee 10 bps of what a step uses with a cap of 30, and 0.01 NEAR per cross-chain signature with a cap of 0.05 NEAR. The caps are fixed for good; the admin moves the fees within them with `set_fee`. Register the treasury on every token vaults trade (`storage_deposit`), or token fees stay owed in each vault until someone calls `settle_fees` after it is registered. Passing no `fees` deploys a brain that can never charge any.
- Relay v2 (unpublished before this): the brain's first round is 40 Tgas (was 100) and the vault's `cb` floor 5 (was 10), so every library node, Borrow on Rhea's 60 + 120 included, runs within mainnet's 300 Tgas; and a vault can close itself (`set_auto_close`, `close`), its name tombstoned at the factory.
