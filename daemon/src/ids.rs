//! Random suffixes for file names.
//!
//! Nothing here is load-bearing for security — file names are defence in depth, and [`crate::paths`]
//! re-validates every name before it is turned into a path. The randomness only keeps two captures
//! in the same second apart.

use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// How many bytes of `/dev/urandom` to read at most before falling back to a weaker mix.
const URANDOM_BYTES: usize = 8;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// `hex_chars` lowercase hex characters, from `/dev/urandom` when it is readable.
pub fn random_hex(hex_chars: usize) -> String {
    if hex_chars == 0 {
        return String::new();
    }
    let wanted = hex_chars.div_ceil(2).min(URANDOM_BYTES);
    let mut bytes = vec![0u8; wanted];
    let from_urandom = std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_ok();
    if !from_urandom {
        // A weak but unique-enough mix: nanoseconds, pid and a process-wide counter.
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let mut seed = nanos
            ^ (u64::from(std::process::id()) << 32)
            ^ COUNTER.fetch_add(1, Ordering::Relaxed)
            ^ 0x9e37_79b9_7f4a_7c15;
        for byte in bytes.iter_mut() {
            // xorshift64*: deterministic given the seed, good enough for a file-name suffix.
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            *byte = (seed >> 24) as u8;
        }
    }
    let mut out = String::with_capacity(hex_chars);
    for index in 0..hex_chars {
        let byte = bytes[(index / 2) % bytes.len()];
        out.push(if index % 2 == 0 {
            hex_digit(byte >> 4)
        } else {
            hex_digit(byte & 0x0f)
        });
    }
    out
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => char::from(b'0' + value),
        _ => char::from(b'a' + (value - 10)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn length_and_alphabet() {
        for len in [0usize, 1, 2, 3, 6, 7, 16] {
            let hex = random_hex(len);
            assert_eq!(hex.len(), len);
            assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(hex.bytes().all(|b| !b.is_ascii_uppercase()));
        }
    }

    #[test]
    fn values_differ() {
        let a = random_hex(6);
        let b = random_hex(6);
        assert_ne!(a, b, "two suffixes in a row should differ");
    }
}
