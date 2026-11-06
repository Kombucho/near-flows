# NEAR Flows demo

One command, one vault, three chains:

    scripts/demo/run.sh

It builds the contracts, starts a local NEAR node and two local mainnet forks, then runs
`contracts/sandbox/tests/demo.rs` and prints only its narrative. About 2 minutes.
Nothing is sent to a real network.

Alice has one **relay vault**: a 10 KB keyless contract that forwards every request to
the shared **brain**, which holds her rules. The keeper (a bot) builds the transactions
and asks the vault to run them, always as a step of one of Alice's flows. The vault checks each one against Alice's rules, signs
it, and refuses anything else.

| Leg | What happens | What gets refused |
|---|---|---|
| 1. NEAR | A flow wraps 10 NEAR, then buys MEME on Rhea. The minimum comes from the oracle (NEAR from Pyth, MEME from the price board), at most 1% under the market. | A whale pumps the pool just before the buy. The vault's minimum is above what the pool pays, so Rhea reverts and the wNEAR stays in the flow. Once the whale sells back, the buy goes through. |
| 2. Solana | A one-node flow holds 0.05 SOL; the keeper steps it with a pump.fun buy, checked against Alice's template (this coin only, at most 0.1 SOL, a coins-per-lamport floor, coins delivered to the vault's own account) and the node (it spends exactly what it holds). | The keeper asking the vault to sign directly, outside any flow (only the owner may). A 0.06 SOL buy (more than the node holds). |
| 3. Solana | Alice co-signs a Meteora DLMM position (100 USDC). A trader's 100 SOL sell earns it fees. A flow runs `[sweep fees] → 100% → [pump.fun buy]`. The sweep is claim_fee2 plus closing wSOL in one transaction, so the fees land as SOL. The keeper reports what came in, and the ledger routes it to the buy node, which spends exactly that. | An open without Alice's wallet. A fee report over the node's 0.5 SOL cap. A buy spending 1 lamport more than the node holds. |
| 4. Robinhood Chain | A flow buys NVDA with 0.03 ETH on Uniswap v4 and sends a third to each rung. Each rung sells when the oracle reaches 1.5x / 2x / 3x of $180, and each sale's ETH is zapped into StonkBrokers' ETHNVDA pair token. At the end the fork's balances must equal the ledger. | Each rung at $1 under its price. |

## Real vs simulated

**Real**
- Protocol code. Rhea DEX and wNEAR run their real testnet wasm (`scripts/fetch-protocols.sh`). On the forks, pump.fun, Meteora DLMM, Uniswap v4, Permit2 and StonkBrokers' ETHNVDA are the real mainnet programs and contracts, with their mainnet state (pools, bonding curve, liquidity) downloaded at the moment the fork starts.
- Our contracts. The relay vault, brain, relay factory and price board are the same wasm we deploy, built by `make build`.
- Transactions. Every Solana and EVM transaction is built by the protocols' own SDKs (keeper helper `scripts/keeper/src/fork-sol.ts`, or `cast`), checked byte by byte by the brain, signed, and executed by the fork. The amounts printed are read back from the forks.
- Price board logic. It checks the 2-of-3 Ed25519 attestations and keeps the median.

**Simulated or stood in**
- The MPC signer. `mock_signer` holds real secp256k1 / Ed25519 keys and signs the way NEAR's MPC network would. Only the key custody differs. The derived addresses are the mock's own, not mainnet MPC addresses.
- Oracle prices. `mock_pyth` has Pyth's read interface and the test sets the prices: NEAR $2, then NVDA walking $180 → $545. MEME's $5.1204e-8 is a real pump.fun curve price from earlier, signed by test attester keys.
- MEME on NEAR is a mock token (6 decimals). Its Rhea pool is seeded at the oracle price.
- Balances on the forks are set with cheat codes: 2 SOL, 500 USDC, 1 ETH. The whale and the trader are test accounts.
- Remote outputs are keeper-reported. The vault can't see Solana or EVM balances, so it accepts a report only between the minimum it signed and the node's cap. In the demo the keeper reports what the fork shows.
- The Meteora fees are tiny (around 0.00005 SOL) because the position is 100 USDC in a pool of about $4M. The amount depends on mainnet state when the fork starts, so it changes from run to run.

## Notes
- Ports: anvil 9500 (Robinhood Chain), surfpool 9510 / 9511 (Solana, public RPC). The script stops if a port is busy, and the forks are killed when the test ends.
- The public Solana RPC rate-limits account downloads. On "Failed to fetch accounts", run it again.
- `ROBINHOOD_RPC` overrides Robinhood Chain's public RPC.
- `SKIP_BUILD=1` reuses the built wasm. `VERBOSE=1` shows the full test output. The full log is always in `contracts/target/demo.log`.
