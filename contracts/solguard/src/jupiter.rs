//! Jupiter v6's route arguments, decoded the way the program decodes them.
//!
//! `route(route_plan: Vec<RoutePlanStep>, in_amount: u64, quoted_out_amount: u64,
//! slippage_bps: u16, platform_fee_bps: u8)`; `shared_accounts_route` has an
//! `id: u8` first. Anchor reads the arguments front to back and ignores any
//! bytes after them, so the amounts are where the route plan ENDS, not the
//! last 19 bytes of the data: a decoy tail would otherwise be checked while
//! Jupiter runs the real numbers. Here the plan is walked step by step, and the
//! data must end exactly after the amounts.
//!
//! Each step is `{swap: Swap, percent: u8, input_index: u8, output_index: u8}`;
//! `Swap` is an enum of ~200 AMMs, most without fields. Its layout comes from
//! the on-chain IDL (account C88XWfp26heEmDkmfSzeXP7Fd7GQJ2j9dDTUsyiZbUTa, read
//! 2026-11-06, 204 variants). A variant we don't know (Jupiter adds them at the
//! end) or can't size safely (the Dynamic routes nest another enum) is refused.

/// Bytes of each `Swap` variant's fields, by variant index. `V` = variable, decoded in `swap_args`.
const V: u8 = 0xff;
#[rustfmt::skip]
const SWAP_ARGS: [u8; 204] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 1, 1, 1, 1, 0,
    0, 1, 0, 1, 1, 0, 0, 1, 1, 16, 0, 0, 0, 4, 0, 0, 0, 0, 0, 1,
    0, 4, 3, 10, 5, 5, 0, V, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0,
    1, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, V, 0, 0, 0, 0,
    0, 8, 8, 0, 0, 1, 2, 9, 0, 1, 0, 0, 0, 0, 1, 1, 0, 0, 0, 0,
    0, 0, 0, V, 1, 0, 1, 1, 0, 0, 1, V, 0, 0, 0, 0, 1, 1, 9, 1,
    V, 1, 16, 48, 0, 1, 17, 1, 0, 1, 0, 0, 1, 0, 0, 10, 1, 0, 0, 0,
    0, 1, 0, 0, 0, 1, V, 0, 0, 0, 0, 1, 1, 1, 0, 2, 0, 9, 0, 8,
    1, 5, 1, 0, 1, 1, 1, 2, 1, 0, 1, 1, 1, 0, 1, 0, 0, 1, 1, 0,
    0, 1, 1, V, 1, 57, 65, 2, 0, 1, 81, 2, 1, 1, 1, 0, 1, 1, 0, 1,
    0, 0, 0, 1,
];

/// Most steps one route may have (Jupiter's routes use a handful).
const MAX_STEPS: u32 = 16;

/// The amounts at the end of a route instruction.
pub(crate) struct RouteArgs {
    pub in_amount: u64,
    pub quoted_out: u64,
    pub slippage_bps: u16,
    pub platform_fee_bps: u8,
}

struct Reader<'a> {
    d: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self.at.checked_add(n).filter(|e| *e <= self.d.len()).ok_or("route data cut short")?;
        let b = &self.d[self.at..end];
        self.at = end;
        Ok(b)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    /// Borsh `Option`: 0 = none, 1 = some (anything else Jupiter refuses too).
    fn some(&mut self) -> Result<bool, String> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err("bad option tag".into()),
        }
    }
    /// `RemainingAccountsInfo { slices: Vec<{accounts_type: u8, length: u8}> }`.
    fn remaining_accounts_info(&mut self) -> Result<(), String> {
        let n = self.u32()? as usize;
        self.take(n.checked_mul(2).ok_or("route data cut short")?)?;
        Ok(())
    }
}

/// Skip one `Swap` value's fields.
fn swap_args(r: &mut Reader, variant: u8) -> Result<(), String> {
    let size = *SWAP_ARGS.get(variant as usize).ok_or_else(|| format!("unknown Jupiter swap variant {variant}"))?;
    if size != V {
        r.take(size as usize)?;
        return Ok(());
    }
    match variant {
        // WhirlpoolSwapV2, DefiTuna: a_to_b: bool, Option<RemainingAccountsInfo>
        47 | 103 => {
            r.take(1)?;
            if r.some()? {
                r.remaining_accounts_info()?;
            }
        }
        // MeteoraDlmmSwapV2: RemainingAccountsInfo
        75 => r.remaining_accounts_info()?,
        // JupiterRfqV2: side, fill_data: Vec<u8>
        120 => {
            r.take(1)?;
            let n = r.u32()? as usize;
            r.take(n)?;
        }
        // PerenaStarV2TrancheWithdrawFromExternal: kind, Option<u8>
        183 => {
            r.take(1)?;
            if r.some()? {
                r.take(1)?;
            }
        }
        // DynamicV1, DynamicV2: a nested enum of candidates; not decoded, so refused.
        _ => return Err(format!("Jupiter swap variant {variant} isn't supported")),
    }
    Ok(())
}

/// Decode a route instruction's arguments (after the 8-byte discriminator).
/// `has_id`: shared_accounts_route's leading `id: u8`. The data must end
/// exactly after the amounts.
pub(crate) fn route_args(args: &[u8], has_id: bool) -> Result<RouteArgs, String> {
    let mut r = Reader { d: args, at: 0 };
    if has_id {
        r.u8()?;
    }
    let steps = r.u32()?;
    if steps == 0 || steps > MAX_STEPS {
        return Err(format!("{steps} route steps"));
    }
    for _ in 0..steps {
        let variant = r.u8()?;
        swap_args(&mut r, variant)?;
        r.take(3)?; // percent, input_index, output_index
    }
    let in_amount = r.u64()?;
    let quoted_out = r.u64()?;
    let slippage_bps = u16::from_le_bytes(r.take(2)?.try_into().unwrap());
    let platform_fee_bps = r.u8()?;
    if r.at != args.len() {
        return Err(format!("{} bytes after the route's arguments", args.len() - r.at));
    }
    Ok(RouteArgs { in_amount, quoted_out, slippage_bps, platform_fee_bps })
}
