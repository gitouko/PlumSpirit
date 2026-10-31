//! The random number generators Spiderweb's note rules use, reproduced bit for
//! bit: Python's `random.Random(seed)` (MT19937; tumour sides) and NumPy's
//! `default_rng(seed).random()` (SeedSequence + PCG64; claw random, Hz bass
//! noise). Spec: `SPIDERWEB_NOTES_SPEC.md` Appendix A.

/// Python `random.Random(seed)` (integer seeds).
pub struct PyRandom {
    mt: [u32; 624],
    i: usize,
}

impl PyRandom {
    pub fn new(seed: i64) -> PyRandom {
        let mut s = seed.unsigned_abs();
        let mut key = Vec::new();
        loop {
            key.push((s & 0xFFFF_FFFF) as u32);
            s >>= 32;
            if s == 0 {
                break;
            }
        }
        let mut r = PyRandom {
            mt: [0; 624],
            i: 624,
        };
        r.init_genrand(19_650_218);
        let mt = &mut r.mt;
        let (mut i, mut j) = (1usize, 0usize);
        for _ in 0..624.max(key.len()) {
            mt[i] = (mt[i] ^ (mt[i - 1] ^ (mt[i - 1] >> 30)).wrapping_mul(1_664_525))
                .wrapping_add(key[j])
                .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= 624 {
                mt[0] = mt[623];
                i = 1;
            }
            if j >= key.len() {
                j = 0;
            }
        }
        for _ in 0..623 {
            mt[i] = (mt[i] ^ (mt[i - 1] ^ (mt[i - 1] >> 30)).wrapping_mul(1_566_083_941))
                .wrapping_sub(i as u32);
            i += 1;
            if i >= 624 {
                mt[0] = mt[623];
                i = 1;
            }
        }
        mt[0] = 0x8000_0000;
        r
    }

    fn init_genrand(&mut self, s: u32) {
        self.mt[0] = s;
        for i in 1..624 {
            self.mt[i] = 1_812_433_253u32
                .wrapping_mul(self.mt[i - 1] ^ (self.mt[i - 1] >> 30))
                .wrapping_add(i as u32);
        }
        self.i = 624;
    }

    pub fn genrand_u32(&mut self) -> u32 {
        if self.i >= 624 {
            for k in 0..624 {
                let y = (self.mt[k] & 0x8000_0000) | (self.mt[(k + 1) % 624] & 0x7FFF_FFFF);
                let mut v = self.mt[(k + 397) % 624] ^ (y >> 1);
                if y & 1 != 0 {
                    v ^= 0x9908_B0DF;
                }
                self.mt[k] = v;
            }
            self.i = 0;
        }
        let mut y = self.mt[self.i];
        self.i += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9D2C_5680;
        y ^= (y << 15) & 0xEFC6_0000;
        y ^= y >> 18;
        y
    }

    /// `choice((a, b))`.
    pub fn choice2<T: Copy>(&mut self, a: T, b: T) -> T {
        loop {
            let r = self.genrand_u32() >> 30;
            if r < 2 {
                return if r == 0 { a } else { b };
            }
        }
    }

    /// `random()` (53-bit float).
    pub fn random(&mut self) -> f64 {
        let a = self.genrand_u32() >> 5;
        let b = self.genrand_u32() >> 6;
        (f64::from(a) * 67_108_864.0 + f64::from(b)) * (1.0 / 9_007_199_254_740_992.0)
    }
}

/// NumPy `default_rng(seed)` (PCG64 seeded through SeedSequence).
pub struct NpRng {
    state: u128,
    inc: u128,
}

const PCG_MULT: u128 = (2_549_297_995_355_413_924u128 << 64) | 4_865_540_595_714_422_341u128;

impl NpRng {
    pub fn new(seed: u64) -> NpRng {
        // SeedSequence(seed).generate_state(4, uint64)
        let mut entropy = vec![(seed & 0xFFFF_FFFF) as u32];
        if seed >> 32 != 0 {
            entropy.push((seed >> 32) as u32);
        }
        let mut hc: u32 = 0x43b0_d7e5;
        let hashmix = |v: u32, hc: &mut u32| -> u32 {
            let mut v = v ^ *hc;
            *hc = hc.wrapping_mul(0x931e_8875);
            v = v.wrapping_mul(*hc);
            v ^ (v >> 16)
        };
        let mix = |x: u32, y: u32| -> u32 {
            let r = 0xca01_f9ddu32
                .wrapping_mul(x)
                .wrapping_sub(0x4973_f715u32.wrapping_mul(y));
            r ^ (r >> 16)
        };
        let mut pool = [0u32; 4];
        for (i, p) in pool.iter_mut().enumerate() {
            *p = hashmix(entropy.get(i).copied().unwrap_or(0), &mut hc);
        }
        for src in 0..4 {
            for dst in 0..4 {
                if src != dst {
                    let h = hashmix(pool[src], &mut hc);
                    pool[dst] = mix(pool[dst], h);
                }
            }
        }
        for &e in entropy.iter().skip(4) {
            for d in pool.iter_mut() {
                let h = hashmix(e, &mut hc);
                *d = mix(*d, h);
            }
        }
        let mut hb: u32 = 0x8b51_f9dd;
        let mut w = [0u32; 8];
        for (i, x) in w.iter_mut().enumerate() {
            let mut d = pool[i % 4] ^ hb;
            hb = hb.wrapping_mul(0x58f3_8ded);
            d = d.wrapping_mul(hb);
            *x = d ^ (d >> 16);
        }
        let v: Vec<u64> = (0..4)
            .map(|j| u64::from(w[2 * j]) | (u64::from(w[2 * j + 1]) << 32))
            .collect();
        let initstate = (u128::from(v[0]) << 64) | u128::from(v[1]);
        let inc = (((u128::from(v[2]) << 64) | u128::from(v[3])) << 1) | 1;
        let mut r = NpRng { state: 0, inc };
        r.step();
        r.state = r.state.wrapping_add(initstate);
        r.step();
        r
    }

    #[inline]
    fn step(&mut self) {
        self.state = self.state.wrapping_mul(PCG_MULT).wrapping_add(self.inc);
    }

    pub fn next_u64(&mut self) -> u64 {
        self.step();
        let hi = (self.state >> 64) as u64;
        let lo = self.state as u64;
        (hi ^ lo).rotate_right((hi >> 58) as u32)
    }

    /// `random()` in [0, 1).
    pub fn random(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_random_choice_matches_cpython() {
        let mut r = PyRandom::new(1);
        let v: Vec<i32> = (0..12).map(|_| r.choice2(1, -1)).collect();
        assert_eq!(v, vec![1, 1, -1, 1, -1, -1, -1, -1, 1, 1, -1, 1]);
        let mut r = PyRandom::new(0);
        let v: Vec<i32> = (0..12).map(|_| r.choice2(1, -1)).collect();
        assert_eq!(v, vec![-1, -1, 1, -1, -1, -1, -1, -1, -1, 1, 1, -1]);
    }

    #[test]
    fn numpy_default_rng_matches_numpy() {
        let mut r = NpRng::new(1);
        let v: Vec<f64> = (0..4).map(|_| r.random()).collect();
        assert_eq!(
            v,
            vec![
                0.5118216247002567,
                0.9504636963259353,
                0.14415961271963373,
                0.9486494471372439
            ]
        );
        let mut r = NpRng::new(1060);
        let v: Vec<f64> = (0..3).map(|_| r.random()).collect();
        assert_eq!(
            v,
            vec![0.9845042964653733, 0.08682562888663004, 0.9250646896995482]
        );
    }
}
