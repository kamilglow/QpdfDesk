//! Random inputs for the tests of the parsers: a small fixed-seed
//! generator, so a failure repeats on every run — no crate needed for it.

/// xorshift64*: plenty for test inputs, never for anything secret.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.max(1))
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number below `n` (`n` > 0).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    pub fn chance(&mut self, one_in: u64) -> bool {
        self.below(one_in) == 0
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }

    /// Up to `max` characters, mostly from `alphabet`, now and then any
    /// character at all (control, bidirectional, astral…).
    pub fn text(&mut self, alphabet: &[char], max: u64) -> String {
        let len = self.below(max + 1);
        (0..len).map(|_| if self.chance(16) { self.any_char() } else { *self.pick(alphabet) }).collect()
    }

    pub fn any_char(&mut self) -> char {
        let pool = [0x20, 0x80, 0x800, 0x1_0000, 0x11_0000];
        let top = *self.pick(&pool);
        // Surrogates are no `char`: those draws become a control character.
        char::from_u32(self.below(top) as u32).unwrap_or('\u{1}')
    }

    pub fn bytes(&mut self, max: u64) -> Vec<u8> {
        let len = self.below(max + 1);
        (0..len).map(|_| self.next() as u8).collect()
    }
}

/// The characters of `text`, for [`Rng::text`].
pub fn chars(text: &str) -> Vec<char> {
    text.chars().collect()
}
