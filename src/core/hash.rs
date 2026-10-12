//! Small, stable hashing helpers.
//!
//! Fingerprints are persisted to disk, so they must not depend on
//! `std::hash::DefaultHasher`, whose algorithm may change between releases.

/// 64-bit FNV-1a hasher.
#[derive(Debug, Clone, Copy)]
pub struct Fnv64(u64);

impl Fnv64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01b3;

    pub const fn new() -> Self {
        Self(Self::OFFSET)
    }

    pub fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    pub fn hex(self) -> String {
        format!("{:016x}", self.0)
    }
}

impl Default for Fnv64 {
    fn default() -> Self {
        Self::new()
    }
}

/// Hex-encoded FNV-1a hash of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    let mut hasher = Fnv64::new();
    hasher.write(bytes);
    hasher.hex()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_reference_vectors() {
        assert_eq!(hex(b""), "cbf29ce484222325");
        assert_eq!(hex(b"a"), "af63dc4c8601ec8c");
        assert_eq!(hex(b"foobar"), "85944171f73967e8");
    }
}
