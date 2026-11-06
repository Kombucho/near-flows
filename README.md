# NEAR Flows

NEAR Flows lets you write money rules once ("stake 1 NEAR now, DCA 0.5 NEAR into LiNEAR every 10 minutes, buy NVDA on Robinhood Chain and sell a third at each of 1.5x, 2x and 3x") and have them run on their own, across NEAR, Solana and EVM chains. Your funds sit in your own **relay vault**, a small keyless contract. It forwards every request to one shared **brain** that holds your rules and checks each step against them: oracle price floors, caps, approved templates for Solana and EVM transactions signed through NEAR chain signatures. A **keeper** bot runs the steps whose triggers fired. It can run your steps, but it can't withdraw or change anything.

## Run it yourself, free

A keeper can charge a price per run, which you set when you name it. If your vault can't pay that price, the brain refuses the keeper's step up front: the keeper doesn't work for free. You can always run your own keeper, which costs nothing but gas. Three ways in:

1. **Hosted brain, your own keeper.** Create a vault on the hosted relay factory, name yourself keeper at fee 0, and run `npm run flows -- --send` in `scripts/keeper`.
2. **Full self-host.** Deploy your own brain, relay factory and price board to testnet with `scripts/deploy-relay-testnet.sh` (about 20 NEAR of faucet money; see `scripts/DEPLOY-RELAY.md`).
3. **Local demo.** `scripts/demo/run.sh` runs one vault across Rhea on NEAR, pump.fun and Meteora on Solana, and Uniswap v4 on Robinhood Chain, all on local forks. Nothing touches a real network.

Setting it up with an AI coding agent (Claude Code, Codex...)? Point it at **[AGENTS.md](AGENTS.md)**. It has the exact commands, the checks for each step, the costs, and the safety rules (testnet first, keys never in the repo).

## Build and test

```
make build    # contract wasm
make test     # unit tests
make sandbox  # integration tests in a local NEAR node
```
