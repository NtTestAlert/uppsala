//! A small, dependency-free fast hasher for internal hash maps.
//!
//! Used only for internal arena node IDs. XML-derived strings (including
//! entity names and namespace prefixes) must use randomized standard-library
//! hashing to resist collision-driven denial of service. This is the FxHash algorithm
//! (rotate-add-xor with a fixed multiplier), vendored to keep uppsala's
//! zero-dependency property instead of pulling in `rustc-hash`.
//!
//! Not cryptographically secure — do not use for anything security-sensitive.

/// The FxHash multiplier (64-bit).
const FX_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

#[derive(Default, Clone)]
pub struct FxHasher {
    hash: u64,
}

impl FxHasher {
    #[inline]
    fn add_to_hash(&mut self, i: u64) {
        self.hash = (self.hash.rotate_left(5) ^ i).wrapping_mul(FX_SEED);
    }
}

impl std::hash::Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        // Word-at-a-time over the bulk, then the 1-7 byte tail.
        let (bulk, tail) = bytes.as_chunks::<8>();
        for chunk in bulk {
            self.add_to_hash(u64::from_le_bytes(*chunk));
        }
        if !tail.is_empty() {
            let mut buf = [0u8; 8];
            buf[..tail.len()].copy_from_slice(tail);
            self.add_to_hash(u64::from_le_bytes(buf) ^ (tail.len() as u64));
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add_to_hash(i as u64);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add_to_hash(i as u64);
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add_to_hash(i as u64);
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add_to_hash(i);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

/// A `std::hash::BuildHasher` producing [`FxHasher`].
#[derive(Default, Clone, Copy)]
pub struct FxBuildHasher;

impl std::hash::BuildHasher for FxBuildHasher {
    type Hasher = FxHasher;

    fn build_hasher(&self) -> FxHasher {
        FxHasher::default()
    }
}

/// Type alias for the internal fast hash maps.
pub type FastHashMap<K, V> = std::collections::HashMap<K, V, FxBuildHasher>;
pub type FastHashSet<K> = std::collections::HashSet<K, FxBuildHasher>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::Hasher;

    #[test]
    fn usize_hash_is_deterministic() {
        let mut a = FxHasher::default();
        let mut b = FxHasher::default();
        a.write_usize(0xDEAD_BEEF);
        b.write_usize(0xDEAD_BEEF);
        assert_eq!(a.finish(), b.finish());
    }

    #[test]
    fn different_values_differ() {
        let mut a = FxHasher::default();
        let mut b = FxHasher::default();
        a.write_usize(1);
        b.write_usize(2);
        assert_ne!(a.finish(), b.finish());
    }

    #[test]
    fn write_matches_bytes() {
        // write_usize(5) on a 64-bit target hashes the same 8 bytes.
        let mut a = FxHasher::default();
        a.write_usize(5);
        let mut b = FxHasher::default();
        b.write(&5usize.to_le_bytes());
        assert_eq!(a.finish(), b.finish());
    }

    #[test]
    fn map_roundtrip() {
        let mut m = FastHashMap::default();
        m.insert(1u32, "one");
        m.insert(2, "two");
        assert_eq!(m.get(&1), Some(&"one"));
        assert_eq!(m.get(&3), None);
    }
}
