# One-command entry points. Run from the repo root.
.PHONY: build test sandbox sim

# Build the contract wasm files (ABI skipped: the engine's state types carry no schema).
build:
	cd contracts/market && cargo near build non-reproducible-wasm --no-abi
	cd contracts/mock_ft && cargo near build non-reproducible-wasm --no-abi
	cd contracts/bazaar && cargo near build non-reproducible-wasm --no-abi
	cd contracts/mock_nft && cargo near build non-reproducible-wasm --no-abi
	cd contracts/vault && cargo near build non-reproducible-wasm --no-abi
	cd contracts/factory && cargo near build non-reproducible-wasm --no-abi
	cd contracts/mock_dex && cargo near build non-reproducible-wasm --no-abi
	cd contracts/mock_signer && cargo near build non-reproducible-wasm --no-abi
	cd contracts/mock_pyth && cargo near build non-reproducible-wasm --no-abi
	cd contracts/brain && cargo near build non-reproducible-wasm --no-abi
	cd contracts/factory_relay && cargo near build non-reproducible-wasm --no-abi
	cd contracts/mock_brain && cargo near build non-reproducible-wasm --no-abi
	# The relay (vault body) has no near-sdk, so cargo-near won't build it: plain cargo, stripped.
	cd contracts && cargo build -p relay --profile relay --target wasm32-unknown-unknown && mkdir -p target/near/relay && cp target/wasm32-unknown-unknown/relay/relay.wasm target/near/relay/relay.wasm
	cd contracts/priceboard && cargo near build non-reproducible-wasm --no-abi

# Unit tests: math, engine, contract. Two runs: built together, the contract's
# "near" feature would switch the engine's own tests to on-chain storage maps.
test:
	cd contracts && cargo test -p math -p engine
	cd contracts && cargo test -p market
	cd contracts && cargo test -p bazaar
	cd contracts && cargo test -p flow -p vault -p solguard -p priceboard
	cd contracts && cargo test -p relay -p brain

# Integration tests: real wasm in a local NEAR node (downloads the sandbox binary on first run).
sandbox: build
	cd contracts && cargo test -p sandbox -- --test-threads=2

# Stress-test simulator: 300 seeds x 12 regimes, dynamic fee (0.3% base, k=2, max 5%), 230 bins per side.
sim:
	cd contracts && SIM_QUIET=1 cargo run --profile sim -p sim -- 300 30 230 2 500
