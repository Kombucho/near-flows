# Relay deploy (testnet)

`scripts/deploy-relay-testnet.sh` deploys the split design: brain, relay factory, relay code published as a global contract, price board, and one demo vault with oracle feeds and a Rhea flow.

```
SUFFIX=abcde DRY=1 ./scripts/deploy-relay-testnet.sh   # prints every command, sends nothing
SUFFIX=abcde STEP=costs ./scripts/deploy-relay-testnet.sh
SUFFIX=abcde ./scripts/deploy-relay-testnet.sh         # the real run (owner approves first)
```

Stages, in order: `build accounts brain factory publish priceboard vault oracle flow check`. `oracle` also re-points an existing vault's feeds to the board: `SUFFIX=fl06 STEP=oracle ./scripts/deploy-relay-testnet.sh` (one owner call through `x`, ~9 Tgas). You can run one at a time (`STEP=vault`) or a comma list. Before a stage spends anything, it reads the paying account's balance and stops if it's short. The message it prints includes the exact `send-near` command to top the account up.

## What it costs

Storage costs 0.01 NEAR per KB, and a global contract costs 10 times that. Each account starts with 10 NEAR from the faucet. The "measured" column comes from `contracts/sandbox/tests/deploy_sequence.rs`, which replays the script.

| stage | paid by | estimate | measured | what | refundable? |
|---|---|---|---|---|---|
| brain | nf-brain | 8.66 | 8.64 | 860,722 B of code (8.607 locked) + gas | yes: delete the account |
| factory | nf-relay | 1.68 | 1.66 | 165,011 B of code (1.650 locked) + gas | yes: delete the account |
| publish | nf-relay | 1.05 | 1.00 | 9,989 B of relay global code (0.9989, **burned**) | **no** |
| priceboard | nf-board | 2.86 | 2.84 | 282,675 B of code (2.827 locked) + gas | yes: delete the account |
| vault + flow | nf-owner | 6.39 | 6.32 | 1 into the vault + 5 sent to it + token storage 0.0375 + Rhea 0.1 + gas | see below |
| (later) | nf-keeper | 0.1 | 0 | gas for steps and board price pushes (~3 Tgas each, every ~30 s while demoing) | - |

The total is about 20.4 NEAR across 5 faucet accounts. Every account fits inside its own 10 NEAR, so no pooling is needed. The brain is the tightest: about 1.39 NEAR is left after the deploy. It doesn't need more, because each vault pays the brain for its own record (the demo vault paid 0.033 NEAR for a 0.028 NEAR record). If the faucet gives less than 10 NEAR, run with `FAUCET=<n>` and the table will flag what needs topping up.

## Refunds and rollback

- **Brain, factory, board**: `near account delete-account <acc> beneficiary <your account> network-config testnet sign-with-legacy-keychain send` returns the whole balance, storage included. Deleting the brain also strands every vault. The vault records the vaults paid for go to the beneficiary, not back to the vault owners.
- **Relay code**: the ~1 NEAR global deploy is burned. Nothing gets it back, and re-publishing costs it again.
- **Vault**: keyless by design, so it can never be deleted. The owner gets its NEAR back with `x {"drain": ...}` / `x {"withdraw": {"token": "near", "amount": ...}}`. The 0.05 NEAR reserve, its own storage (193 B) and token storage deposits stay with it.
- **Token storage deposits**: these belong to the vault on each token contract. Recovering them needs `storage_unregister`, which the brain never lets a vault call. Treat them as spent (0.04 NEAR plus Rhea's 0.1).
- **Order for a full rollback**: withdraw from the vault first (it needs the brain), then delete the board, factory and brain.

## Price feeds: all five from our price board

The vault reads every feed (`near usdc eth sol nvda`) from `nf-board-$SUFFIX`, each confirmed by the board's own EMA (10-minute window) within 2%, fresh under 60 s, confidence under 1%. Two reasons, both found on 2026-11-06:

1. **Pyth's NEAR receiver is out of date.** `pyth-oracle.testnet` refuses every Hermes update today ("Source for attestation is not allowed"). Read-only check: `pyth-oracle.near` and `pyth-oracle.testnet` both accept only emitter `e101faedac5851e32b9b23b5f9411a8c2bac4aae3ed4dd7b811dd1a72ea4aa71` on chain 26. Today's Hermes update carries emitter `507974686e6574…` ("PythnetPythnet…") on chain 26, signed under guardian set index 1 with 3 signatures. That is Pyth's own signer set since the Aug 2026 Core upgrade, not Wormhole's 19 guardians. Until Pyth updates the NEAR receivers, Pyth-on-NEAR is unusable on mainnet too.
2. **Our Pyth key isn't entitled to NEAR/USD or NVDA** (403 "Not entitled", standard and 24/7 feeds alike). Those come from Chainlink on other chains.

| feed | where from | board price id | keeper command |
|---|---|---|---|
| eth, usdc, sol | Pyth Hermes, relayed | Pyth's own ids | `npm run pyth -- --board $BOARD eth usdc sol` |
| near | Chainlink NEAR / USD on Arbitrum | `0017…` + proxy | `npm run chainlink-price -- --board $BOARD near` |
| nvda | Chainlink RHNVDA / USD on Robinhood Chain | `0048…` + proxy | `npm run chainlink-price -- --board $BOARD nvda` |

A Chainlink price id is the Wormhole chain id (2 bytes), 10 zero bytes, then the feed's proxy address. Anyone can read off the id where a price comes from, and the same id works on an attested board and an Evm board.

Run both from `scripts/keeper` with `KEEPER_ACCOUNT_ID=nf-keeper-$SUFFIX.testnet`. The vault wants prices under 60 s old. You don't need to loop them: `SUFFIX=$SUFFIX scripts/keeper/live.sh` runs the keeper with `--board`, which posts a vault's board prices just before its step goes (nothing due, nothing posted). Gas is about 2-4 Tgas per push. `PYTH_API_KEY` is read from the environment: load it from the repo's `.env` (`node --env-file`, or `set -a; . ../../.env; set +a`). Never print it or commit it. `--dry` sends nothing. `chainlink-price --dry` without `--board` only reads the feeds and needs no NEAR account. `STEP=check` shows each board price.

The demo flow has no market floors on purpose: testnet Rhea pools don't track real prices (1 wNEAR buys about 1.24 testnet USDT), so a floor would refuse every swap.

### The Chainlink feeds (verified 2026-11-06 by eth_call on public RPCs)

| pair | chain (Wormhole id) | proxy | decimals | heartbeat | deviation | read | cross-check |
|---|---|---|---|---|---|---|---|
| NEAR / USD | Arbitrum (23) | `0xBF5C3fB2633e924598A46B9D07a174a9DBcF57C0` | 8 | 24 h | 0.5% | $3.7530 (round 19300, phase 2) | CoinGecko $3.75; the same pair on Optimism $3.7509, BSC $3.7544, Avalanche $3.7421 |
| NEAR / USD (backup) | Optimism (24) | `0xca6fa4b8CB365C02cd3Ba70544EFffe78f63ac82` | 8 | 24 h | 0.5% | $3.7509 | (as above) |
| RHNVDA / USD | Robinhood Chain (72), EVM id 4663 | `0x379EC4f7C378F34a1B47E4F3cbeBCbAC3E8E9F15` | 8 | 24 h, none off-hours | 0.5% | $189.5568 (round 1584) | Chainlink NVDA/USD on Arbitrum $189.5950; token uiMultiplier 1.000775 |

Each proxy's `description()` was checked (`NEAR / USD`, `RHNVDA / USD`), and the keeper checks it again on every read. The Robinhood feed also has an SVR proxy, `0xCF169363636D73dbBf77733629CB38919d14232d`, on the same aggregator. We use the primary one. Ethereum, Base and Polygon list no NEAR/USD feed.

**Which NVDA price: the token's.** The Robinhood feed reports the token's Total Return Value, the stock price × `uiMultiplier()` on the token contract (1.000775 today, from reinvested dividends). The vault holds and trades the *token* (`0xd0601ce1…` on Robinhood Chain), so the token price is what makes a market floor right. It also keeps a ladder's dollar thresholds meaningful through a split. In a 10:1 split the stock drops from $200 to $20, but Robinhood pauses the feed, moves the multiplier to 10 and resumes at $200. A stock-price feed would make every floor 10× too low and every rung unreachable.

**Market hours.** The NVDA feed follows the 24/5 session and has no heartbeat off-hours. Over a weekend its answer gets older than 24 h, and then the keeper won't sign it and an Evm board refuses it ("feed stale or market closed"). No price means no trade on NVDA until the market reopens. That's on purpose.

### Trust: attested now, Wormhole-verified when there's a key

- **Attested board (what this script deploys, k = 1): trusts the keeper.** The keeper relays the price, and nothing on chain checks it against Pyth or Chainlink. Every price can be audited, though. `chainlink-price` logs the chain, the feed, the block, the round id and `updatedAt`; replay `latestRoundData()` at that block to check it. `pyth --board` logs a Hermes URL for the signed update at that `publish_time`. To trust any one keeper less, add attesters (`set_attesters`, k of n; the board keeps their median).
- **Evm board (`source.kind = "evm"`, contract and tests ready): trusts Wormhole's guardians and Chainlink, not the keeper.** Anyone posts a guardian-signed Wormhole Query (`eth_call` of `latestRoundData()` then `decimals()` on a registered feed). The board checks a 13-of-19 quorum, the chain, the address, the call data, the block's age (`max_block_age_s`) and the answer's age at that block (the feed's heartbeat). The price is published at the block's time, with the feed's deviation threshold as its confidence. To get one you need `WORMHOLE_QUERY_API_KEY`, because Wormhole's query proxy only serves allow-listed keys. With it, `npm run chainlink-price -- --board <evm board> near nvda` queries and posts. Deploy the board with guardian set 7 (`contracts/priceboard/tests-data/guardian_set_7.txt`, the set Ethereum's core contract reports) and the feeds above at deviation 50 bps and heartbeat 86400 s, then point the vault's `near`/`nvda` feeds at it (same ids).
- **Pyth prices (ETH, USDC, SOL) stay attested**, and there's no Wormhole path for them: see the design note below.

### Design note: trustless Pyth on NEAR without Pyth's receiver (not built)

The board could check Hermes updates itself, the way the Evm mode checks Wormhole Queries. An accumulator update (`PNAU`) is a signed VAA whose payload is a Merkle root, plus each price's message and Merkle proof. The board would verify the VAA's signatures, check the emitter (chain 26, the new `Pythnet…` address), then check each message's proof against the root (keccak160 in Pyth's accumulator), parse the price message (id, price, conf, expo, publish_time, EMA) and store it. That is roughly the existing `wormhole.rs` reader plus a 20-byte Merkle check.

The catch is who signs. Today's updates carry guardian set index 1 with 3 signatures. That isn't Wormhole's guardian set, so the board would need Pyth's own current signer set and its rotation messages, and we would be trusting Pyth's signers, as Pyth's own receivers do after the upgrade. We should pin that set from Pyth's published contracts (e.g. the EVM receiver's `getCurrentGuardianSetIndex`/`getGuardianSet` on a chain Pyth has updated) before writing it.

## Notes

- The relay is published by calling the factory's `publish` (`call-function ... file-args relay.wasm`), not with `near contract deploy-as-global`. That way the factory records the hash its vaults are created from. The global deploy is charged to the factory's balance. The attached 1.099 NEAR (the wasm size × 0.1 NEAR/KB, plus 0.1) is the factory paying itself, so it can be sure it holds that amount.
- The brain keeps its full-access key on testnet so it can be deleted for the refund. **On mainnet, delete the full-access keys of both the brain and the factory after deploy** (`near account delete-keys <acc> public-keys <key> network-config mainnet sign-with-keychain send`, after checking `list-keys`). A brain key can rewrite every vault's rules and drain them; a factory key can redeploy the factory (new code, a new brain for new vaults) or spend the NEAR it keeps for tombstones. Publish the relay before deleting the factory's key: `publish` is owner-only, and the owner is the factory account here.
- Users can pin the relay they audited: `create_vault` and `create_stealth_vault` take an optional `expected_code_hash` (base58, as `get_code_hash` prints it) and refuse if the published relay differs.
- Each vault leaves 0.005 NEAR of its deposit with the factory for good (`VAULT_SHARE`): its list entry, its owner's entry and one day its tombstone. The brain deletes a vault only after the factory wrote the tombstone, so the factory can never run out of room for one.
- The brain keeps a tombstone of its own for every closed vault (`is_closed`, ~110 bytes out of the record refund) and refuses a record to that name for good, so a creation already past the factory's check can't revive it.
- The brain refuses record growth when its own spare NEAR (beyond its storage) is below 0.01 NEAR (`STORAGE_MARGIN`): keep at least that on it. A vault that leaves its record owing (a read that failed after growing it) is made to pay by anyone with `x {"pay_storage": {}}`.
- The price board starts attested by the keeper's own key with k=1, which means trusting the keeper. Add attesters with `set_attesters` (the owner is nf-owner).
- Fees: the brain's init now takes `fees` (brain/src/fees.rs): treasury and fee admin (both nf-owner here, `TREASURY_ID` overrides), protocol fee 10 bps of what a step uses with a cap of 30, and 0.01 NEAR per cross-chain signature with a cap of 0.05 NEAR. The caps are fixed for good; the admin moves the fees within them with `set_fee`. Register the treasury on every token vaults trade (`storage_deposit`), or token fees stay owed in each vault until someone calls `settle_fees` after it is registered. Passing no `fees` deploys a brain that can never charge any.
- Relay v2 (unpublished before this): the brain's first round is 40 Tgas (was 100) and the vault's `cb` floor 5 (was 10), so every library node, Borrow on Rhea's 60 + 120 included, runs within mainnet's 300 Tgas; and a vault can close itself (`set_auto_close`, `close`), its name tombstoned at the factory.
