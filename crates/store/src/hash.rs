//! Postgres has no unsigned integer type, so every `u64` hash (`url_hash`, `key_hash`,
//! `content_hash`, the `issues` bitmask) crosses the SQL boundary as a `BIGINT` using the
//! bit-pattern cast: same bits, reinterpreted as signed. Round-tripping through these two
//! functions keeps that cast in one place instead of at each call site.

/// `u64` -> the `BIGINT` it is stored as.
pub fn to_db(value: u64) -> i64 {
    value.cast_signed()
}

/// The `BIGINT` read back from storage -> the original `u64`.
pub fn from_db(value: i64) -> u64 {
    value.cast_unsigned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn round_trips_zero_and_max() {
        assert_eq!(from_db(to_db(0)), 0);
        assert_eq!(from_db(to_db(u64::MAX)), u64::MAX);
    }

    proptest! {
        #[test]
        fn round_trips_arbitrary_u64(value: u64) {
            prop_assert_eq!(from_db(to_db(value)), value);
        }
    }
}
