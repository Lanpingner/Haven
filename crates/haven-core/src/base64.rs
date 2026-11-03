//! Just enough base64 for `OSC 52`, which is the only thing here that needs it.
//!
//! A dependency for sixty lines that cannot change would be a poor trade, and
//! the clipboard payload is the one place a terminal meets base64 at all.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        // The tail is padded rather than truncated: a terminal on the other
        // end is entitled to reject an unpadded payload.
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Decode, or `None` if the input is not base64 at all.
///
/// Whitespace is skipped, because payloads arriving over a pty have been
/// wrapped by something often enough to be worth tolerating.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in text.chars() {
        if c == '=' {
            break;
        }
        if c.is_whitespace() {
            continue;
        }
        let value = ALPHABET.iter().position(|a| *a as char == c)? as u32;
        acc = (acc << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_tail_length() {
        // The padding cases are where every hand-written base64 goes wrong.
        for text in ["", "a", "ab", "abc", "abcd", "abcde", "hello haven"] {
            let encoded = encode(text.as_bytes());
            assert_eq!(decode(&encoded).as_deref(), Some(text.as_bytes()), "{text:?}");
        }
    }

    #[test]
    fn matches_the_known_answers() {
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
    }

    #[test]
    fn non_ascii_survives() {
        let text = "héllo → haven";
        assert_eq!(decode(&encode(text.as_bytes())).as_deref(), Some(text.as_bytes()));
    }

    #[test]
    fn rubbish_is_rejected_rather_than_guessed_at() {
        assert_eq!(decode("not base64!"), None);
    }

    #[test]
    fn wrapped_payloads_are_tolerated() {
        assert_eq!(decode("Zm9v\n").as_deref(), Some(b"foo".as_slice()));
    }
}
