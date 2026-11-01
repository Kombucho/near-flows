//! Seeded randomness, so every run can be replayed exactly from its seed.

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }

    /// Uniform in [0, 1).
    pub fn f(&mut self) -> f64 {
        // xorshift64*: small, fast, good enough for simulation.
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Standard normal (Box-Muller).
    pub fn normal(&mut self) -> f64 {
        let (u1, u2) = (self.f().max(1e-12), self.f());
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }

    /// Number of events this hour for an average `rate` per hour (Poisson).
    pub fn poisson(&mut self, rate: f64) -> u32 {
        let (limit, mut p, mut k) = ((-rate).exp(), 1.0, 0);
        loop {
            p *= self.f();
            if p <= limit {
                return k;
            }
            k += 1;
        }
    }

    pub fn pick(&mut self, n: usize) -> usize {
        (self.f() * n as f64) as usize % n
    }
}
