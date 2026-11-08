/** Limits read from origin/main 9608b9df on 2026-11-08. Not a claim about an unpushed revision. */
export const SOURCE_COMMIT = "9608b9df7a7ec6f13eb10e28fc0aabd297ff12a7";

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
