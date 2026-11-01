# One-command entry points. Run from the repo root.
.PHONY: build test sim

# Build the market contract wasm (ABI skipped: the engine's state types carry no schema).
build:
	cd contracts/market && cargo near build non-reproducible-wasm --no-abi

# Unit tests: math, engine, contract.
test:
	cd contracts && cargo test -p math -p engine -p market

# Stress-test simulator: 300 seeds x 12 regimes, 1% fee, 230 bins per side.
sim:
	cd contracts && SIM_QUIET=1 cargo run --profile sim -p sim -- 300 100 230
