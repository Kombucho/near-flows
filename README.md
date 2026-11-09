# NEAR Flows

NEAR Flows lets you write money rules once ("stake 1 NEAR now, DCA 0.5 NEAR into LiNEAR every 10 minutes, buy NVDA on Robinhood Chain and sell a third at each of 1.5x, 2x and 3x") and have them run on their own, across NEAR, Solana and EVM chains. Your funds sit in your own **relay vault**, a small keyless contract. It forwards every request to one shared **brain** that holds your rules and checks each step against them: oracle price floors, caps, approved templates for Solana and EVM transactions signed through NEAR chain signatures. A **keeper** bot runs the steps whose triggers fired. It can run your steps, but it can't withdraw or change anything.

## Run it yourself, free

A keeper can charge a price per run, which you set when you name it. If your vault can't pay that price, the brain refuses the keeper's step up front: the keeper doesn't work for free. You can always run your own keeper, which costs nothing but gas. Three ways in:

1. **Hosted brain, your own keeper.** Create a vault on the hosted relay factory, name yourself keeper at fee 0, and run `npm run flows -- --send` in `scripts/keeper`.
2. **Full self-host.** Deploy your own brain, relay factory and price board to testnet with `scripts/deploy-relay-testnet.sh` (about 24 NEAR of faucet money; see `scripts/DEPLOY-RELAY.md`).
3. **Local demo.** `scripts/demo/run.sh` runs one vault across Rhea on NEAR, pump.fun and Meteora on Solana, and Uniswap v4 on Robinhood Chain, all on local forks. Nothing touches a real network.

Setting it up with an AI coding agent (Claude Code, Codex...)? Point it at **[AGENTS.md](AGENTS.md)**. It has the exact commands, the checks for each step, the costs, and the safety rules (testnet first, keys never in the repo).

## Live on testnet

Deployed 2026-11-09 with `SUFFIX=fl08 scripts/deploy-relay-testnet.sh` (testnet NEAR only, no real value):

| | Account |
|---|---|
| Brain (all rules, shared) | [`nf-brain-fl08.testnet`](https://testnet.nearblocks.io/address/nf-brain-fl08.testnet) |
| Relay factory (`FACTORY_ID`) | [`nf-relay-fl08.testnet`](https://testnet.nearblocks.io/address/nf-relay-fl08.testnet) |
| Relay code (global, by hash) | `7ePdZqrLYWp68y4hDYFofu43hnX19HkGPxGWi2MTn1N4` |
| Price board | [`nf-board-fl08.testnet`](https://testnet.nearblocks.io/address/nf-board-fl08.testnet) |
| Demo vault | [`demo.nf-relay-fl08.testnet`](https://testnet.nearblocks.io/address/demo.nf-relay-fl08.testnet) |

The demo vault's flow ran on the real testnet contracts through the keeper: 1 NEAR staked with LiNEAR (0.4378 LiNEAR), 2 NEAR wrapped and split into two Rhea swaps (about 1.24 testnet USDT, and 157,867.9 of a meme.cooking coin), and a DCA into LiNEAR every 10 minutes (0.1 NEAR per run, 0.4 NEAR left). The 0.1% protocol fee was taken on each step.

Prices: Pyth's NEAR receiver currently rejects Pyth's post-2026-08-26 updates ("Source for attestation is not allowed"), so vaults read our own price boards instead. What is checked on chain and what isn't:

- **ETH, USDC, SOL (Pyth): verified on chain.** A `pyth_core` board takes Hermes's signed update as it is and checks it itself: 3 of the 5 signers in Pyth's own signer set (pinned at deploy, rotated only by that set's own upgrade VAAs), the Pythnet emitter, and each price's Merkle proof. The keeper only carries the bytes; it can't change a price, only fail to post one. (Deployed by `scripts/deploy-relay-testnet.sh` as `nf-pyth-<suffix>`; fl08 above predates it and still has these on the attested board.)
- **Their Chainlink backups, NEAR and its backup, NVDA: keeper-vouched.** The keeper reads Chainlink on Arbitrum, Optimism and Robinhood Chain and signs what it read, with every source logged for audit. They become verified on chain when the board reads Chainlink through Wormhole Queries (the Evm board, built, waiting for a query API key).

They are posted on demand, just before a step, and each feed is checked against a second, independent publisher (Chainlink on another chain); NVDA has no second source and its vault says so (no market floor on it, fixed floor and size caps apply). With a verified Pyth price and a keeper-vouched backup, a dishonest keeper can make a vault refuse a trade, but can't move the price an ETH, USDC or SOL floor is computed from.

## Build and test

```
make build    # contract wasm
make test     # unit tests
make sandbox  # integration tests in a local NEAR node
```
