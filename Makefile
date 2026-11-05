# One-command entry points. Run from the repo root.
.PHONY: build test sandbox sim

# Build the contract wasm files (ABI skipped: the engine's state types carry no schema).
build:
	cd contracts/market && cargo near build non-reproducible-wasm --no-abi
	cd contracts/mock_ft && cargo near build non-reproducible-wasm --no-abi
	cd contracts/bazaar && cargo near build non-reproducible-wasm --no-abi
	cd contracts/mock_nft && cargo near build non-reproducible-wasm --no-abi

# Unit tests: math, engine, contract. Two runs: built together, the contract's
# "near" feature would switch the engine's own tests to on-chain storage maps.
test:
	cd contracts && cargo test -p math -p engine
	cd contracts && cargo test -p market
	cd contracts && cargo test -p bazaar

# Integration tests: real wasm in a local NEAR node (downloads the sandbox binary on first run).
sandbox: build
	cd contracts && cargo test -p sandbox -- --test-threads=2

# Stress-test simulator: 300 seeds x 12 regimes, dynamic fee (0.3% base, k=2, max 5%), 230 bins per side.
sim:
	cd contracts && SIM_QUIET=1 cargo run --profile sim -p sim -- 300 30 230 2 500
