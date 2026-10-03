//! Severity and the per-page issue bitmask. The check list itself lives in the checks crate.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Critical,
    Warning,
    Notice,
}

/// One bit per check, stored as `pages.issues`. Bit numbers are never reused.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IssueBits(pub u64);

impl IssueBits {
    pub fn set(&mut self, bit: u8) {
        assert!(bit < 64, "check bit {bit} out of range");
        self.0 |= 1 << bit;
    }

    pub fn has(&self, bit: u8) -> bool {
        bit < 64 && self.0 & (1 << bit) != 0
    }

    pub fn iter(&self) -> impl Iterator<Item = u8> + '_ {
        (0..64u8).filter(|b| self.has(*b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_bits_set_and_test() {
        let mut b = IssueBits::default();
        b.set(0);
        b.set(63);
        assert!(b.has(0));
        assert!(b.has(63));
        assert!(!b.has(1));
        assert_eq!(b.iter().collect::<Vec<_>>(), vec![0, 63]);
    }

    #[test]
    #[should_panic(expected = "check bit 64 out of range")]
    fn issue_bit_64_is_rejected() {
        IssueBits::default().set(64);
    }
}
