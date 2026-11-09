# Relay deploy (testnet)

`scripts/deploy-relay-testnet.sh` deploys the split design: brain, relay factory, relay code published as a global contract, price board, and one demo vault with oracle feeds and a Rhea flow.

```
SUFFIX=abcde DRY=1 ./scripts/deploy-relay-testnet.sh   # prints every command, sends nothing
SUFFIX=abcde STEP=costs ./scripts/deploy-relay-testnet.sh
SUFFIX=abcde ./scripts/deploy-relay-testnet.sh         # the real run (owner approves first)
```

Stages, in order: `build accounts brain factory publish priceboard pythboard vault oracle flow check`. `PYTH_SOURCE=core` (the default) deploys `nf-pyth-$SUFFIX`, a second board that checks Pyth's signed updates on chain, and points the vault's ETH/USDC/SOL at it; `PYTH_SOURCE=attested` skips it and puts them on the keeper-attested `nf-board` (the old way). `oracle` also re-points an existing vault's feeds to the board: `SUFFIX=fl08 STEP=oracle ./scripts/deploy-relay-testnet.sh` (one owner call through `x`, ~9 Tgas). You can run one at a time (`STEP=vault`) or a comma list. Before a stage spends anything, it reads the paying account's balance and stops if it's short. The message it prints includes the exact `send-near` command to top the account up.

## What it costs

Storage costs 0.01 NEAR per KB, and a global contract costs 10 times that. Each account starts with 10 NEAR from the faucet. The "measured" column comes from `contracts/sandbox/tests/deploy_sequence.rs`, which replays the script.

| stage | paid by | estimate | measured | what | refundable? |
|---|---|---|---|---|---|
| brain | nf-brain | 9.55 | 9.51 | 950,089 B of code (9.501 locked) + gas | yes: delete the account |
| factory | nf-relay | 1.74 | 2.78 with publish | 170,795 B of code (1.708 locked) + gas | yes: delete the account |
| publish | nf-relay | 1.12 | (above) | 10,697 B of relay global code (1.0697, **burned**) | **no** |
| priceboard | nf-board | 2.99 | 2.97 | 296,141 B of code (2.969 locked) + gas | yes: delete the account |
| pythboard (`PYTH_SOURCE=core`) | nf-pyth | 2.99 | 2.97 | the same code (2.969 locked) + gas | yes: delete the account |
| vault + flow | nf-owner | 6.39 | 6.12 | 1 into the vault + 5 sent to it + token storage 0.0375 + Rhea 0.1 + gas | see below |
| (later) | nf-keeper | 0.1 | 0 | gas for steps and board price posts (3-5 Tgas each, only just before a step) | - |

Measured on 2026-11-09. The total is about 24.4 NEAR across 6 faucet accounts (21.4 across 5 with `PYTH_SOURCE=attested`). Every account fits inside its own 10 NEAR, so no pooling is needed. The brain is the tightest: about 0.49 NEAR is left after the deploy. It doesn't need more, because each vault pays the brain for its own record (the demo vault paid 0.033 NEAR for a 0.028 NEAR record). If the faucet gives less than 10 NEAR, run with `FAUCET=<n>` and the table will flag what needs topping up.

## Refunds and rollback

- **Brain, factory, board**: `near account delete-account <acc> beneficiary <your account> network-config testnet sign-with-legacy-keychain send` returns the whole balance, storage included. Deleting the brain also strands every vault. The vault records the vaults paid for go to the beneficiary, not back to the vault owners.
- **Relay code**: the ~1 NEAR global deploy is burned. Nothing gets it back, and re-publishing costs it again.
- **Vault**: keyless by design, so it can never be deleted. The owner gets its NEAR back with `x {"drain": ...}` / `x {"withdraw": {"token": "near", "amount": ...}}`. The 0.05 NEAR reserve, its own storage (193 B) and token storage deposits stay with it.
- **Token storage deposits**: these belong to the vault on each token contract. Recovering them needs `storage_unregister`, which the brain never lets a vault call. Treat them as spent (0.04 NEAR plus Rhea's 0.1).
- **Order for a full rollback**: withdraw from the vault first (it needs the brain), then delete the board, factory and brain.

## Price feeds: all five from our price boards, each backed by a different publisher

The vault reads Pyth's three (`eth usdc sol`) from `nf-pyth-$SUFFIX`, a **pyth_core** board that checks Pyth's signed updates itself, and the rest (`near nvda` and every backup) from `nf-board-$SUFFIX`, attested by the keeper. With `PYTH_SOURCE=attested` there is no `nf-pyth` and all five come from `nf-board` (the old, keeper-vouched way). Every feed must be fresh under 60 s, confidence under 1%. A feed that sets a market floor must be confirmed by its **backup: the same asset from a different publisher**, read in the same round and within 2%. The board keeps the latest price per feed and nothing else: **no average of its own, and no periodic posting**. Prices go up on demand, just before a step (the keeper with `--board` posts a vault's feeds and their backups, then sends the step). WHY not an average: with on-demand posts, an average of the board's own prices is only as honest as the poster's timing, and it is the same publisher checked against its own past. Two independent publishers read at the same moment have to be wrong together.

| feed | primary | backup (different publisher) |
|---|---|---|
| eth | Pyth ETH/USD (Hermes's signed update, checked on chain) | Chainlink ETH/USD on Arbitrum (`eth-arb`) |
| sol | Pyth SOL/USD (Hermes's signed update, checked on chain) | Chainlink SOL/USD on Arbitrum (`sol-arb`) |
| usdc | Pyth USDC/USD (Hermes's signed update, checked on chain) | Chainlink USDC/USD on Arbitrum (`usdc-arb`) |
| near | Chainlink NEAR/USD on Arbitrum | Chainlink NEAR/USD on Optimism (`near-op`): a separate proxy, aggregator and update stream |
| nvda | Chainlink RHNVDA/USD on Robinhood Chain | the 30-min TWAP of the NVDA/USDG 0.05% Uniswap v3 pool on Robinhood Chain (`nvda-twap`) |

**NVDA's backup is a pool, not a second oracle.** No second independent NVDA token price exists from an oracle on a chain we can read (Chainlink's NVDA-USD 24/5 on Optimism is the stock, without the token's 1.000775 multiplier, and comes from the same publisher). The NVDA/USDG pool `0xd4eb…14a3` (factory `0x1f7d…2EfA`'s 500 pool; ~9,300 NVDA + ~147k USDG, 7,200 observations) is a different source: `observe([1800, 0])` gives its 30-minute mean tick, which one block can't move and half an hour of manipulation costs the pool's depth. It only CONFIRMS Chainlink's price (within the vault's 2% tolerance): every floor is computed from Chainlink's, so a bent TWAP can make a vault refuse, never trade cheaper. USDG is taken as $1. On 2026-11-09 the TWAP read $178.72 against Chainlink's $179.02 (-0.17%), and $178.56 against $178.07 later that morning (+0.28%). When the stock market is closed Chainlink's feed pauses, so nothing reads NVDA and nothing trades in it: that's correct. The id is Chainlink's scheme with the pool's address, so an Evm board could check a Wormhole Query of `observe` later; today the keeper attests it (`npm run twap-price`).

**pump.fun curve coins: no market floor.** No trusted price exists for them: anyone can buy into a curve and have it read at that slot, and Jupiter's API is unsigned. The board can carry their price (`jupiter-price`, `wormhole-price`) for triggers, but a vault names such a feed `single_source`, so no market floor reads it: the owner's fixed floor, max slippage and smaller size caps bound the trade. Graduated memecoins (DEX TWAP oracle accounts) are out of scope for now.

Two reasons for the board, both found on 2026-11-06:

1. **Pyth's NEAR receiver is out of date.** `pyth-oracle.testnet` refuses every Hermes update today ("Source for attestation is not allowed"). Read-only check: `pyth-oracle.near` and `pyth-oracle.testnet` both accept only emitter `e101faedac5851e32b9b23b5f9411a8c2bac4aae3ed4dd7b811dd1a72ea4aa71` on chain 26. Today's Hermes update carries emitter `507974686e6574…` ("PythnetPythnet…") on chain 26, signed under guardian set index 1 with 3 signatures. That is Pyth's own signer set since the Aug 2026 Core upgrade, not Wormhole's 19 guardians. Until Pyth updates the NEAR receivers, Pyth's own contract on NEAR is unusable on mainnet too, so our pyth_core board does that check instead (below).
2. **Our Pyth key isn't entitled to NEAR/USD or NVDA** (403 "Not entitled", standard and 24/7 feeds alike). Those come from Chainlink on other chains.

| feed | where from | board price id | keeper command |
|---|---|---|---|
| eth, usdc, sol | Pyth Hermes: the signed update as is, on `nf-pyth` (or relayed and attested, on `nf-board`) | Pyth's own ids | `npm run pyth -- --board $PYTH_BOARD eth usdc sol` |
| their backups | Chainlink ETH, SOL, USDC / USD on Arbitrum | `0017…` + proxy | `npm run chainlink-price -- --board $BOARD eth-arb sol-arb usdc-arb` |
| near | Chainlink NEAR / USD on Arbitrum | `0017…` + proxy | `npm run chainlink-price -- --board $BOARD near` |
| near's backup | Chainlink NEAR / USD on Optimism | `0018…` + proxy | `npm run chainlink-price -- --board $BOARD near-op` |
| nvda | Chainlink RHNVDA / USD on Robinhood Chain | `0048…` + proxy | `npm run chainlink-price -- --board $BOARD nvda` |
| nvda's backup | NVDA/USDG Uniswap v3 30-min TWAP on Robinhood Chain | `0048…` + pool | `npm run twap-price -- --board $BOARD nvda-twap` |

`--vault <vault>` instead of names posts exactly the feeds and backups that vault reads from the board (what the keeper does before a step). The keeper takes both boards at once: `npm run flows -- --send --board nf-pyth-$SUFFIX.testnet,nf-board-$SUFFIX.testnet` (`live.sh` adds `nf-pyth` when the deployment has one). Each script posts only what that kind of board takes.

A Chainlink price id is the Wormhole chain id (2 bytes), 10 zero bytes, then the feed's proxy address. Anyone can read off the id where a price comes from, and the same id works on an attested board and an Evm board.

Run both from `scripts/keeper` with `KEEPER_ACCOUNT_ID=nf-keeper-$SUFFIX.testnet`. The vault wants prices under 60 s old. Don't loop them: `SUFFIX=$SUFFIX scripts/keeper/live.sh` runs the keeper with `--board`, which posts a vault's board prices (each feed and its backup) just before its step goes, and nothing in between (nothing due, nothing posted). Between steps `STEP=check` showing stale prices is normal. Gas is about 2-4 Tgas per push. `PYTH_API_KEY` is read from the environment: load it from the repo's `.env` (`node --env-file`, or `set -a; . ../../.env; set +a`). Never print it or commit it. `--dry` sends nothing. `chainlink-price --dry` without `--board` only reads the feeds and needs no NEAR account. `STEP=check` shows each board price.

The demo flow has no market floors on purpose: testnet Rhea pools don't track real prices (1 wNEAR buys about 1.24 testnet USDT), so a floor would refuse every swap.

### The Chainlink feeds (verified by eth_call on public RPCs)

Addresses, heartbeats and deviation thresholds are from Chainlink's reference data directory (`https://reference-data-directory.vercel.app/feeds-ethereum-mainnet-arbitrum-1.json`, the primary, non-SVR proxy of each pair; Optimism's and Robinhood Chain's likewise). Each was then read with `description()`, `decimals()` and `latestRoundData()` (`npm run chainlink-price -- --dry <names>` repeats the read, no account needed).

| pair | chain (Wormhole id) | proxy | decimals | heartbeat | deviation | read | cross-check |
|---|---|---|---|---|---|---|---|
| NEAR / USD | Arbitrum (23) | `0xBF5C3fB2633e924598A46B9D07a174a9DBcF57C0` | 8 | 24 h | 0.5% | $3.7530 (round 19300, phase 2) | CoinGecko $3.75; the same pair on Optimism $3.7509, BSC $3.7544, Avalanche $3.7421 |
| NEAR / USD (backup) | Optimism (24) | `0xca6fa4b8CB365C02cd3Ba70544EFffe78f63ac82` | 8 | 24 h | 0.5% | $3.7509 | (as above) |
| RHNVDA / USD | Robinhood Chain (72), EVM id 4663 | `0x379EC4f7C378F34a1B47E4F3cbeBCbAC3E8E9F15` | 8 | 24 h, none off-hours | 0.5% | $189.5568 (round 1584) | Chainlink NVDA/USD on Arbitrum $189.5950; token uiMultiplier 1.000775 |
| ETH / USD (ETH's backup) | Arbitrum (23) | `0x639Fe6ab55C921f74e7fac1ee960C0B6293ba612` | 8 | 1755 s | 0.05% | $2234.1570 (2026-11-08, phase 2 round 152732) | Pyth (Hermes) $2234.0571 |
| SOL / USD (SOL's backup) | Arbitrum (23) | `0x24ceA4b8ce57cdA5058b924B9B9987992450590c` | 8 | 24 h | 0.1% | $89.1871 (2026-11-08, phase 2 round 74172) | Pyth (Hermes) $89.1423 |
| USDC / USD (USDC's backup) | Arbitrum (23) | `0x50834F3163758fcC1Df9973b6e91f0F0F0434aD3` | 8 | 255 s | 0.1% | $0.99980 (2026-11-08, phase 3 round 25455) | Pyth (Hermes) $0.9998 |

Each proxy's `description()` was checked (`NEAR / USD`, `RHNVDA / USD`), and the keeper checks it again on every read. The Robinhood feed also has an SVR proxy, `0xCF169363636D73dbBf77733629CB38919d14232d`, on the same aggregator. We use the primary one. Ethereum, Base and Polygon list no NEAR/USD feed.

**Which NVDA price: the token's.** The Robinhood feed reports the token's Total Return Value, the stock price × `uiMultiplier()` on the token contract (1.000775 today, from reinvested dividends). The vault holds and trades the *token* (`0xd0601ce1…` on Robinhood Chain), so the token price is what makes a market floor right. It also keeps a ladder's dollar thresholds meaningful through a split. In a 10:1 split the stock drops from $200 to $20, but Robinhood pauses the feed, moves the multiplier to 10 and resumes at $200. A stock-price feed would make every floor 10× too low and every rung unreachable.

**Market hours.** The NVDA feed follows the 24/5 session and has no heartbeat off-hours. Over a weekend its answer gets older than 24 h, and then the keeper won't sign it and an Evm board refuses it ("feed stale or market closed"). No price means no trade on NVDA until the market reopens. That's on purpose.

### Trust: what's checked on chain, what's keeper-vouched

| price | where | checked on chain against | trusts |
|---|---|---|---|
| ETH, USDC, SOL (Pyth) | `nf-pyth`, pyth_core | Pyth's signers (3 of its 5), the Pythnet emitter, each price's Merkle proof | Pyth (its 5 routers), as Pyth's own receivers do. **Not the keeper.** |
| their Chainlink backups (`-arb`) | `nf-board`, attested | nothing | **the keeper** |
| NEAR, its backup `near-op` | `nf-board`, attested | nothing | **the keeper** |
| NVDA, its backup `nvda-twap` | `nf-board`, attested | nothing | **the keeper** |

So a market floor on ETH, USDC or SOL now rests on a price nobody but Pyth could have signed. Its backup still comes from the keeper: a lying keeper can make the vault REFUSE a trade (a backup more than 2% off), but it can no longer move the price the floor is computed from. NEAR and NVDA floors still rest on keeper-vouched prices until the Evm board (below) has a key.

- **pyth_core board (`nf-pyth`, `source.kind = "pyth_core"`): trusts Pyth's signers, not the keeper.** Anyone posts Hermes's update bytes as they are (`post_update`). The board checks that a majority (3 of 5) of its signer set signed the VAA, that the VAA is from Pythnet (chain 26, emitter `5079…7468`), that each price message is proven under the signed Merkle root, that the feed is listed (`set_pyth_feeds`, owner), the price is positive and at most `max_age_s` (60) old, and stores only what's newer. Pyth's own confidence is kept. The signer set is pinned at deploy (set 1, the 5 addresses Pyth's wormhole receivers on Arbitrum and Base report) and moves only by an upgrade VAA that set signed (`upgrade_guardians`, anyone). The owner can't change the signers or the emitter. About 3.6 Tgas to post one feed, 5.0 Tgas for three (0.0005 NEAR); a live Hermes update for three took 4.9 Tgas in the sandbox.
- **Attested board (`nf-board`, k = 1): KEEPER-VOUCHED.** The keeper relays the price, and nothing on chain checks it against Chainlink (or Pyth, with `PYTH_SOURCE=attested`). Both a feed and its backup come through the same keeper, so "two publishers agree" protects against one publisher being wrong, not against the keeper lying about both. That stays true until the board reads Chainlink itself (Evm mode, below). Every price can be audited, though. `chainlink-price` logs the chain, the feed, the block, the round id and `updatedAt`; replay `latestRoundData()` at that block to check it. `pyth --board` logs a Hermes URL for the signed update at that `publish_time`. To trust any one keeper less, add attesters (`set_attesters`, k of n; the board keeps their median).
- **Evm board (`source.kind = "evm"`, contract and tests ready, waiting for `WORMHOLE_QUERY_API_KEY` in `NEAR/.env`): trusts Wormhole's guardians and Chainlink, not the keeper.** Anyone posts a guardian-signed Wormhole Query (`eth_call` of `latestRoundData()` then `decimals()` on a registered feed). The board checks a 13-of-19 quorum, the chain, the address, the call data, the block's age (`max_block_age_s`) and the answer's age at that block (the feed's heartbeat). The price is published at the block's time, with the feed's deviation threshold as its confidence. To get one you need `WORMHOLE_QUERY_API_KEY`, because Wormhole's query proxy only serves allow-listed keys. With it, `npm run chainlink-price -- --board <evm board> near nvda` queries and posts. Deploy the board with guardian set 7 (`contracts/priceboard/tests-data/guardian_set_7.txt`, the set Ethereum's core contract reports) and the Chainlink feeds above at their deviation and heartbeat, then point the vault's `near`/`nvda` feeds and the `-arb`/`-op` backups at it (same ids). With that, every backup is trustless too, and nothing is keeper-vouched.

### How the pyth_core board checks a Hermes update (contracts/priceboard/src/pythcore.rs)

An accumulator update (`PNAU`, major 1, update type 0) is a VAA whose payload is a Merkle root (`AUWV`, Pythnet slot, root of 20 bytes), then each price's message (85 bytes: id, price, conf, expo, publish_time, prev publish time, EMA price and conf) with its Merkle proof (12 nodes today). The tree is Pyth's keccak160 one: leaf = keccak256(0x00 ‖ message)[..20], node = keccak256(0x01 ‖ lower ‖ higher)[..20]. The VAA is signed over keccak256(keccak256(body)) with secp256k1 (`env::ecrecover`, the same reader the Evm board uses).

Who signs, as read on 2026-11-09: since the Aug 2026 Core upgrade, Pyth's 5 routers, as **guardian set 1** of Pyth's wormhole receivers (`getCurrentGuardianSetIndex()` = 1 and `getGuardianSet(1)` on Arbitrum `0x8d289cdd…46f8` and Base `0x581aaf05…6c55`, the `wormhole()` of Pyth's Core contracts there, identical on both):

```
0 0x41534bb176e461a3fb30479400f210549ecce638
1 0x6502987b62f21cab7eb5ccd8f0173084b60d5b41
2 0x44a3e8f6a382412cf6bb90a3f8106e68977476c9
3 0x13edc776d3063549fdb0702af182edc905a539d4
4 0x3af088854bb768065f4929e5bbdfa4cbb04c9af9
```

**Quorum: 3 of 5** (more than half, not Wormhole's 2/3 + 1, which would be 4). Pyth's docs say 3 of 5, and Arbitrum's receiver agrees: `parseAndVerifyVM` takes a real update's VAA with its 3 signatures and answers "no quorum" with 2 of them. Every update read so far was signed by routers 1, 2 and 4. Set 0 (routers 3 and 4 different) installed set 1 with a governance VAA (chain 1, address `0x…04`, module "Core", action 2) that 4 of 5 signed; it's in pyth-crosschain's `contract_manager/src/store/guardian_sets/ProCompatibleProductionGuardianSetVaas.json`, and the board's tests replay it. The real updates, both sets and that VAA are in `contracts/priceboard/tests-data/pyth_core.json`.

## Notes

- The relay is published by calling the factory's `publish` (`call-function ... file-args relay.wasm`), not with `near contract deploy-as-global`. That way the factory records the hash its vaults are created from. The global deploy is charged to the factory's balance. The attached 1.099 NEAR (the wasm size × 0.1 NEAR/KB, plus 0.1) is the factory paying itself, so it can be sure it holds that amount.
- The brain keeps its full-access key on testnet so it can be deleted for the refund. **On mainnet, delete the full-access keys of both the brain and the factory after deploy** (`near account delete-keys <acc> public-keys <key> network-config mainnet sign-with-keychain send`, after checking `list-keys`). A brain key can rewrite every vault's rules and drain them; a factory key can redeploy the factory (new code, a new brain for new vaults) or spend the NEAR it keeps for tombstones. Publish the relay before deleting the factory's key: `publish` is owner-only, and the owner is the factory account here.
- Users can pin the relay they audited: `create_vault` and `create_stealth_vault` take an optional `expected_code_hash` (base58, as `get_code_hash` prints it) and refuse if the published relay differs.
- Each vault leaves 0.005 NEAR of its deposit with the factory for good (`VAULT_SHARE`): its list entry, its owner's entry and one day its tombstone. The brain deletes a vault only after the factory wrote the tombstone, so the factory can never run out of room for one.
- The brain keeps a tombstone of its own for every closed vault (`is_closed`, ~110 bytes out of the record refund) and refuses a record to that name for good, so a creation already past the factory's check can't revive it.
- The brain refuses record growth when its own spare NEAR (beyond its storage) is below 0.01 NEAR (`STORAGE_MARGIN`): keep at least that on it. A vault that leaves its record owing (a read that failed after growing it) is made to pay by anyone with `x {"pay_storage": {}}`.
- The attested price board (`nf-board`) starts with the keeper's own key, k=1, which means trusting the keeper (keeper-vouched prices). Add attesters with `set_attesters` (the owner is nf-owner). The pyth_core board (`nf-pyth`) trusts no keeper: its owner (nf-owner) can only change which Pyth feeds it keeps (`set_pyth_feeds`), and `set_owner(null)` gives even that up.
- The board's init takes `owner` and `source` only (no `ema_window_s`: it keeps no average). Its state layout changed with that, so a board deployed before must be redeployed (a fresh account, or delete and recreate), then `STEP=oracle` re-points the vault.
- Fees: the brain's init now takes `fees` (brain/src/fees.rs): treasury and fee admin (both nf-owner here, `TREASURY_ID` overrides), protocol fee 10 bps of what a step uses with a cap of 30, and 0.01 NEAR per cross-chain signature with a cap of 0.05 NEAR. The caps are fixed for good; the admin moves the fees within them with `set_fee`. Register the treasury on every token vaults trade (`storage_deposit`), or token fees stay owed in each vault until someone calls `settle_fees` after it is registered. Passing no `fees` deploys a brain that can never charge any.
- Relay v2 (unpublished before this): the brain's first round is 40 Tgas (was 100) and the vault's `cb` floor 5 (was 10), so every library node, Borrow on Rhea's 60 + 120 included, runs within mainnet's 300 Tgas; and a vault can close itself (`set_auto_close`, `close`), its name tombstoned at the factory.
