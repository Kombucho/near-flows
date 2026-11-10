/** Limits read from origin/main 6afddea on 2026-11-08. */
export const SOURCE_COMMIT = "6afddea58e785a45b80a0e27ec78bafce6dab694";

export const LIMITS = {
  maxNodes: 32,
  maxRoutes: 8,
  maxBps: 10_000,
  maxFlows: 16,
  minEveryNs: 60_000_000_000n,
  maxFailures: 3,
  sources: [
    "contracts/flow/src/lib.rs MAX_NODES MAX_ROUTES BPS MIN_EVERY_NS MAX_FAILURES",
    "contracts/brain/src/lib.rs MAX_FLOWS",
  ],
} as const;
