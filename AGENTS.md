# AGENTS.md: setting up NEAR Flows for a user

This file is for AI coding agents (Claude Code, Codex, Cursor and the like) asked to set NEAR Flows up for someone. Every command runs from the repo root unless it says otherwise. Each step ends with a check: run it, and don't go on until it passes.

NEAR Flows in one paragraph: a user has a **relay vault**, a small keyless contract that holds their funds and forwards every request to one shared **brain** contract that holds their rules (flows of steps: swaps, stakes, DCA, take-profit ladders, steps on Solana and EVM chains signed through NEAR chain signatures). A **keeper** is a bot that watches the flows and asks the vault to run the steps whose triggers fired. The vault and brain check every step against the user's rules, so a keeper can run steps but can't withdraw or edit anything.

## Safety rules (read these first)

1. **Testnet first, always.** Don't broadcast anything to mainnet (`NEAR_NETWORK=mainnet`, `network-config mainnet`, a mainnet RPC) unless the user has said so explicitly in this conversation, for that specific action. "Set it up" doesn't count as a yes to mainnet.
2. **Dry runs before sends.** The keeper sends nothing without `--send`. The deploy script sends nothing with `DRY=1`. Run the dry version first, show the user what it would do, then send.
3. **Keys never enter the repo.** near-cli keeps keys in `~/.near-credentials/<network>/<account>.json` (the legacy keychain) or the OS keychain. The keeper reads `KEEPER_PRIVATE_KEY` from the environment or that file. `.env`, `.env.*` (except `.env.example`), `.near-credentials/`, `neardev/` and `*.pem` are gitignored. Never commit them, print a private key, paste one into chat, or put one in a command line that gets logged.
4. **Hidden-flow plans stay on the user's machine.** `~/.flows/hidden/<vault>.json` holds the plan behind a hidden flow; only its fingerprints go on chain. Don't commit it, upload it or paste it anywhere. Share it only with the user's own keeper.
5. **Kill only processes you started.** The local demo and the sandbox tests start a NEAR node and forks. Stop them by PID, not with `pkill`.
6. **Say what costs money.** Before any send, state who pays, how much, and whether it's refundable (see the costs table below).

## Prerequisites

| tool | why | install | check |
|---|---|---|---|
| rustup + Rust 1.99.1 + `wasm32-unknown-unknown` | contracts | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh`. The pinned toolchain and target come from `rust-toolchain.toml` on the first `cargo` run in the repo (or `rustup toolchain install`) | `rustup show active-toolchain` shows `1.99.1` |
| cargo-near | builds the wasm | `cargo install --locked cargo-near` | `cargo near --version` |
| Node.js 22 + npm | keeper scripts | from nodejs.org, or `fnm`/`nvm` | `node --version` is v22 or newer |
| near-cli-rs | accounts, deploys, calls | `npm install -g near-cli-rs` (or `cargo install near-cli-rs`) | `near --version` |
| python3 | the deploy script's JSON and RPC helpers | system python is fine | `python3 --version` |
| Foundry (anvil, cast) | **only** for the EVM forks (demo, `*_fork*` tests) | `curl -L https://foundry.paradigm.xyz \| bash && foundryup` | `anvil --version` |
| surfpool | **only** for the Solana forks (demo, `sol_fork*` tests) | see https://surfpool.run | `surfpool --version` |

Then install the keeper's packages. Use `--ignore-scripts`: some Solana SDK dependencies run install scripts the keeper doesn't need, and they can fail or hang.

```
cd scripts/keeper && npm install --ignore-scripts
```

Check: `ls scripts/keeper/node_modules/near-api-js` exists.

## Build and test

```
make build    # every contract's wasm, into contracts/target/near/<name>/<name>.wasm
make test     # unit tests (math, engine, market, bazaar, flow, solguard, priceboard, relay, brain)
cd scripts/keeper && npm test    # keeper unit tests (pure logic, no network)
```

Checks:
- `ls contracts/target/near/brain/brain.wasm contracts/target/near/relay/relay.wasm contracts/target/near/factory_relay/factory_relay.wasm` lists all three.
- `make test` and `npm test` end with no failures (`test result: ok` lines, `# fail 0`).

Optional, slower: `make sandbox` (it runs `make build` first) runs the integration tests against the real wasm in a local NEAR node. It downloads the sandbox binary on its first run. One file at a time: `cd contracts && cargo test -p sandbox --test relay_fees`.

## Pick a path

| | what you get | costs the user | needs |
|---|---|---|---|
| (a) hosted brain, your own keeper | your vault on the shared brain, steps run by a keeper you run | vault deposit (yours to withdraw) + gas. **No keeper fee.** | a NEAR account, Node |
| (b) full self-host | your own brain, factory, price board and vault | ~20 NEAR of testnet faucet money (table below) | 5 faucet accounts |
| (c) local demo | the whole thing on local forks, nothing on a real network | nothing | Foundry + surfpool |

### Why run your own keeper

A vault can name a keeper and a price per run (`keeper_fee`, set only by the owner, at most 0.05 NEAR). The vault pays that price after each step the keeper ran successfully. **No free work:** if the vault's free NEAR can't cover the price, the brain refuses the keeper's step up front (nothing moves, the vault isn't left busy) with:

```
keeper fee unpaid: ... top up NEAR, or run your own keeper for free: https://github.com/Kombucho/near-flows (AGENTS.md)
```

and a paid keeper skips that vault with the same pointer. The owner can always run a step themselves. A keeper you run yourself, with `keeper_fee` 0, costs nothing but its gas. Path (a) sets that up.

## (a) Hosted brain, your own keeper (free)

You need the hosted relay factory's account id (`FACTORY_ID`). It is published by the maintainers in README.md. If none is published yet, use path (b): your own factory works the same way.

The commands below use `ME=<the user's testnet account>` and `NAME=<a vault name, free under the factory>`. Make sure the account exists and its key is in the keychain:

```
near account view-account-summary $ME network-config testnet now
ls ~/.near-credentials/testnet/$ME.json
```

No account yet? Create one from the faucet (key saved to the legacy keychain, not the repo):

```
near account create-account sponsor-by-faucet-service $ME autogenerate-new-keypair save-to-legacy-keychain network-config testnet create
```

**1. Create the vault, with yourself as keeper.** A vault made this way starts with `keeper_fee` 0. The attached NEAR (at least 0.1) becomes the vault's own float; 1 NEAR is plenty for testing.

```
near contract call-function as-transaction $FACTORY_ID create_vault json-args "{\"name\":\"$NAME\",\"keeper\":\"$ME\"}" prepaid-gas '300.0 Tgas' attached-deposit '1 NEAR' sign-as $ME network-config testnet sign-with-legacy-keychain send
```

Check: the factory knows it, and the brain's record has you as keeper at fee 0.

```
near contract call-function as-read-only $FACTORY_ID get_vault json-args "{\"owner\":\"$ME\"}" network-config testnet now
BRAIN=$(near contract call-function as-read-only $FACTORY_ID get_brain json-args '{}' network-config testnet now | tail -1 | tr -d '"')
near contract call-function as-read-only $BRAIN get_vault json-args "{\"vault\":\"$NAME.$FACTORY_ID\"}" network-config testnet now
```

The record shows `"keeper": "<ME>"` and `"keeper_fee": "0"`. (If `BRAIN` comes out empty, read the brain's id off the `get_brain` output by hand.)

**Already have a vault with a paid keeper?** Switch it to your own at fee 0 (every vault request goes through the vault's `x` door as `{"<method>": args}`):

```
VAULT=$NAME.$FACTORY_ID
near contract call-function as-transaction $VAULT x json-args "{\"set_keeper\":{\"keeper\":\"$ME\",\"keeper_fee\":\"0\"}}" prepaid-gas '300.0 Tgas' attached-deposit '0 NEAR' sign-as $ME network-config testnet sign-with-legacy-keychain send
```

A separate keeper account works too (and is safer: give it a function-call key that may only call `x` on the vault). Put that account in `keeper` instead of `$ME`.

**2. Fund it and save a flow.** Send NEAR to the vault, trust the contracts the flow calls, save the flow, fund its nodes. The `vault` and `flow` stages of `scripts/deploy-relay-testnet.sh` do exactly this for a demo flow (token registrations, `trust`, `save_flow` for a LiNEAR stake, a wrap, Rhea swaps and a DCA, then `fund`). Copy the argument shapes from there, and swap in your own vault. Don't run those stages for path (a): they create a vault on the script's own factory. Check after each call: `get_vault` on the brain shows the flow, and `get_allocated` (`{"vault": ..., "token": "near"}`) shows what the flows hold.

**3. Run the keeper.** Env vars only (the scripts don't read `.env` on their own, so load it):

```
cd scripts/keeper
cp .env.example .env    # then fill in FACTORY_ID, KEEPER_ACCOUNT_ID=$ME; .env is gitignored
set -a; . ./.env; set +a
npm run flows -- --once            # dry run: prints what is due, sends nothing
npm run flows -- --send --once     # one real round
npm run flows -- --send            # keep running (every 30 s; --tick <s> to change)
npm run flows -- --send --board <board>   # also post the board prices (and backups) a vault reads, just before its step; never on a timer
```

Check: the dry run's last line reads `1 of N vault(s) name this keeper, 0 step(s) sent (dry run)` and lists your due steps. After `--send`, the steps show up on https://testnet.nearblocks.io/address/<vault>, and the brain's `get_vault` shows the nodes' `runs` going up and `inbox` going down. The key comes from `KEEPER_PRIVATE_KEY` or `~/.near-credentials/testnet/$KEEPER_ACCOUNT_ID.json`.

**4. Hidden flows (optional).** Keep the plan local:

```
OWNER_ACCOUNT_ID=$ME npm run hidden -- save $VAULT nodes.json --name "my plan"
```

The plan goes to `~/.flows/hidden/$VAULT.json` and only fingerprints go on chain. The keeper reads that same file, so run it on the same machine (or copy the file to the keeper's machine yourself, never through the repo). Flows the keeper has no plan for are skipped, not failed.

## (b) Full self-host

`scripts/deploy-relay-testnet.sh` deploys a brain, a relay factory, the relay code (published as a global contract), a price board and a demo vault, on **testnet**. Read `scripts/DEPLOY-RELAY.md` first. Pick a fresh `SUFFIX` (account names must be free).

```
SUFFIX=abcde DRY=1 ./scripts/deploy-relay-testnet.sh        # prints every command, sends nothing
SUFFIX=abcde STEP=costs ./scripts/deploy-relay-testnet.sh   # the funding table
SUFFIX=abcde STEP=build,accounts ./scripts/deploy-relay-testnet.sh
SUFFIX=abcde STEP=brain ./scripts/deploy-relay-testnet.sh   # then factory, publish, priceboard, vault, flow
SUFFIX=abcde STEP=check ./scripts/deploy-relay-testnet.sh   # views only
```

Stages, in order: `build accounts brain factory publish priceboard vault flow check`. Show the user the DRY output and the costs table and get their OK before the first stage that sends. Each stage checks the paying account's balance first and stops if it's short, printing the `send-near` command to top it up. `OWNER_ID`, `KEEPER_ID`, `TREASURY_ID`, `VAULT_NAME`, `VAULT_DEPOSIT`, `VAULT_FUND` and `FAUCET` override the defaults.

What it costs (NEAR, testnet faucet money; from DEPLOY-RELAY.md, measured by replaying the script in the sandbox):

| stage | paid by | about | refundable? |
|---|---|---|---|
| brain | nf-brain | 8.64 (8.607 locked by code) | yes: delete the account |
| factory | nf-relay | 1.66 (1.650 locked) | yes: delete the account |
| publish (relay global code) | nf-relay | 1.00 | **no: burned** |
| priceboard | nf-board | 2.53 (2.529 locked) | yes: delete the account |
| vault + flow | nf-owner | 6.32 (1 into the vault, 5 sent to it, token and Rhea storage, gas) | the vault's NEAR: withdraw it. Its 0.05 reserve, its own storage and token storage deposits (~0.14) stay |
| later | nf-keeper | gas for steps and the price posts just before them | - |

About 20.4 NEAR in total across 5 faucet accounts, each within its own 10 NEAR. Rollback order: withdraw from the vault first (it needs the brain), then delete the board, factory and brain (`near account delete-account <acc> beneficiary <yours> network-config testnet sign-with-legacy-keychain send`).

Checks: `STEP=check` prints `factory.get_brain` = your brain, `vault.brain_id` = your brain, a `get_code_hash`, the vault's record cost and what it paid, and each feed's price on the price board (every feed is read from there: Pyth's NEAR receiver refuses today's updates). Every feed that sets a market floor has a backup from a different publisher (Pyth's ETH/SOL/USDC backed by Chainlink on Arbitrum, Chainlink NEAR on Arbitrum backed by Optimism); NVDA has none and is set up `single_source` (it fires triggers, no market floor reads it: fixed floor + size caps apply). The board keeps no average and nothing posts on a timer: prices go up on demand, just before a step (`npm run flows -- --send --board <board>`), so stale prices between steps are normal. A step without fresh prices refuses with "no price fresher than 60 s" (that's correct). To post by hand: `npm run pyth -- --board <board> --vault <vault>` and `npm run chainlink-price -- --board <board> --vault <vault>` (DEPLOY-RELAY.md, "Price feeds"). The script's board is attested by the keeper, so its prices are keeper-vouched until a Wormhole Query API key makes the Evm board possible.

Your brain's fees are set at deploy (`FEES` in the script: 10 bps, cap 30; 0.01 NEAR per cross-chain signature, cap 0.05). To run a brain that never charges anything, deploy it with `"fees": null`. Then run your keeper as in path (a) with `FACTORY_ID=nf-relay-$SUFFIX.testnet`.

## (c) Local demo

```
scripts/demo/run.sh               # build, then run (~2 min); prints only the narrative
SKIP_BUILD=1 scripts/demo/run.sh  # reuse the built wasm
VERBOSE=1 scripts/demo/run.sh     # full test output too
```

It starts a local NEAR sandbox, an anvil fork of Robinhood Chain (port 9500) and a surfpool fork of Solana (9510/9511), and runs `contracts/sandbox/tests/demo.rs`. Nothing goes to a real network. `scripts/demo/README.md` explains each leg and what's real and what's simulated.

Check: the narrative ends without a failure, and `contracts/target/demo.log` has the full log. The forks are stopped when the test ends; if one is left running, the script refuses to start ("port ... is busy"). Then find its PID with `lsof -iTCP:9500 -sTCP:LISTEN` (or 9510/9511) and stop that process only.

## Troubleshooting

- **A sandbox test or the demo behaves like old code** (a fixed error still shows, a new method "doesn't exist"): the wasm is stale. Run `make build`, then the test again. `make sandbox` always builds first; `cargo test -p sandbox` alone doesn't.
- **`Failed to fetch accounts` / 429 from Solana**: surfpool downloads mainnet accounts from the public RPC, which rate-limits. Wait a minute and run again.
- **`npm install` fails or hangs in a native build step**: use `npm install --ignore-scripts` in `scripts/keeper`.
- **`attach at least N Tgas`**: vault requests need up to 300 Tgas. Pass `prepaid-gas '300.0 Tgas'`.
- **`keeper fee unpaid`**: the vault names a paid keeper and its free NEAR is below the price. Top the vault up, or switch to your own keeper at fee 0 (path (a), `set_keeper`).
- **`refused: the signature fee is ...`**: cross-chain signatures cost the brain's flat fee from the vault's free NEAR. Top the vault up.
- **`no key for <account>`** from the keeper: set `KEEPER_PRIVATE_KEY`, or save the key with near-cli to `~/.near-credentials/testnet/<account>.json` (`save-to-legacy-keychain`).
- **The keeper says `0 of N vault(s) name this keeper`**: `KEEPER_ACCOUNT_ID` isn't the vault's keeper. Check the brain's `get_vault` and fix it with `set_keeper`.
- **`a step is running`**: a step is in flight, or one ran out of gas halfway. Wait for it to finish; the owner can clear a stuck one with `unstick`.
- **cargo-near wants an update**: the message is harmless; the build used the installed version.
