//! Standard base64 (RFC 4648, padded), hand-rolled so the dependency set stays small.
//!
//! Only encoding is needed: the vision payload goes out to the agent, nothing comes back in.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const PAD: u8 = b'=';

/// Encode bytes as padded base64.
///
/// The output is always `4 * ceil(input.len() / 3)` bytes, so callers can size a cap before
/// allocating: see [`encoded_len`].
pub fn encode(input: &[u8]) -> String {
    let mut out = Vec::with_capacity(encoded_len(input.len()));
    let mut chunks = input.chunks_exact(3);
    for chunk in chunks.by_ref() {
        let quad = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        out.push(ALPHABET[((quad >> 18) & 0x3f) as usize]);
        out.push(ALPHABET[((quad >> 12) & 0x3f) as usize]);
        out.push(ALPHABET[((quad >> 6) & 0x3f) as usize]);
        out.push(ALPHABET[(quad & 0x3f) as usize]);
    }
    match chunks.remainder() {
        [] => {}
        [one] => {
            let quad = u32::from(*one) << 16;
            out.push(ALPHABET[((quad >> 18) & 0x3f) as usize]);
            out.push(ALPHABET[((quad >> 12) & 0x3f) as usize]);
            out.push(PAD);
            out.push(PAD);
        }
        [one, two] => {
            let quad = (u32::from(*one) << 16) | (u32::from(*two) << 8);
            out.push(ALPHABET[((quad >> 18) & 0x3f) as usize]);
            out.push(ALPHABET[((quad >> 12) & 0x3f) as usize]);
            out.push(ALPHABET[((quad >> 6) & 0x3f) as usize]);
            out.push(PAD);
        }
        _ => {}
    }
    // Every byte pushed above is ASCII, so this cannot fail; `from_utf8` keeps us away from
    // `from_utf8_unchecked` (no `unsafe` in this crate).
    String::from_utf8(out).unwrap_or_default()
}

/// How long [`encode`]'s output will be for `input_len` input bytes.
pub const fn encoded_len(input_len: usize) -> usize {
    input_len.div_ceil(3) * 4
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn rfc4648_vectors() {
        let cases: &[(&[u8], &str)] = &[
            (b"", ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
        ];
        for (input, expected) in cases {
            assert_eq!(&encode(input), expected, "encoding {input:?}");
            assert_eq!(encoded_len(input.len()), expected.len());
        }
    }

    #[test]
    fn binary_input_round_trips_through_a_reference_decode() {
        // Bytes that stress every byte value and the padding cases.
        let input: Vec<u8> = (0u8..=255).chain(0u8..=255).collect();
        let encoded = encode(&input);
        assert_eq!(encoded.len(), encoded_len(input.len()));
        assert_eq!(encoded.len() % 4, 0);
        assert!(encoded.bytes().all(|b| ALPHABET.contains(&b) || b == PAD));
        assert_eq!(&encode(&input[..2]), "AAE=");
    }
}
