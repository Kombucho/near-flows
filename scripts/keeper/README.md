# Keepers

`npm install --ignore-scripts`, then `npm test` (pure logic, no network). Setup for a user: AGENTS.md.

| command | what |
|---|---|
| `npm run flows` | the flow keeper, dry run: lists what is due, sends nothing |
| `npm run flows -- --send` | runs due NEAR steps of the vaults that name this keeper |
| `npm run flows -- --send --remote` | also runs remote nodes: EVM and Solana templates, and hops out of Intents |
| `npm run flows -- --send --board <board>` | posts the board prices a vault reads just before its step |
| `npm run hidden -- save <vault> nodes.json` | a hidden flow's plan, kept in `~/.flows/hidden/` |
| `npm run hop -- ...` | read-only Intents lookups, dry-run quotes, withdrawal status (scripts/HOP.md) |
| `npm run keeper` | the market keeper (crank, settle) |

Options for `flows`: `--once` (one round), `--tick <s>` (default 30).

## Environment

| variable | for |
|---|---|
| `FACTORY_ID`, `KEEPER_ACCOUNT_ID` | the relay factory and the keeper account (required) |
| `NEAR_NETWORK`, `NEAR_RPC` | default testnet, FastNEAR |
| `KEEPER_PRIVATE_KEY` | otherwise `~/.near-credentials/<network>/<keeper>.json` |
| `PYTH_API_KEY` | Pyth prices for `--board` |
| `EVM_RPC_<chainid>` | where `--remote` broadcasts and measures on an EVM chain (4663 Robinhood Chain, 8453 Base) |
| `SOLANA_RPC` | the same on Solana |
| `FLOWS_KEEPER_DIR` | in-flight remote steps, default `~/.flows/keeper/` |
| `REMOTE_SLIPPAGE_BPS` | under a Uniswap v4 quote, default 100 |
| `SOL_SLIPPAGE_PCT` | under a pump.fun quote, default 5 |
| `REMOTE_WAIT_MIN`, `HOP_WAIT_MIN` | when to tell the owner a transaction (30) or a hop (180) hasn't landed |
| `HOP_BRIDGE` | `1` asks the bridge API about hops too (default on mainnet only) |
| `V4_QUOTER_<chainid>` | a Uniswap v4 quoter where the keeper knows none |
| `KEEPER_DEBUG` | `1`: a line each round for every remote step still waiting to land |

## Remote nodes

Code: `src/remote.ts` (plans, builders, the state file; pure, tested in `remote.test.ts`),
`src/remote-run.ts` (step, broadcast, measure, report, resume), `src/sol-build.ts` (Solana messages
from the protocols' SDKs, shared with `fork-sol.ts`). What each template gets, and what's still
manual: AGENTS.md, path (a) step 5. End to end on forks: `contracts/sandbox/tests/keeper_remote_fork.rs`.

The keeper never chooses an amount, a recipient, a nonce or a fee cap: the brain pins them. It never
reports a remote step failed: only the owner may, after looking on the other chain.
