//! One-time bootstrap codes. Only their digest and monotonic issue time are retained.

use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

pub const VALID_HOURS: u64 = 24;
pub const LIFETIME: Duration = Duration::from_secs(VALID_HOURS * 60 * 60);
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

pub struct SetupCode {
    digest: [u8; 32],
    issued_at: Instant,
}

impl std::fmt::Debug for SetupCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SetupCode { <redacted> }")
    }
}

impl SetupCode {
    pub fn issue(now: Instant) -> (Self, String) {
        let mut bytes = [0u8; 10];
        getrandom::fill(&mut bytes).expect("operating system random source");
        let mut text = String::with_capacity(19);
        for i in 0..16 {
            if i > 0 && i % 4 == 0 {
                text.push('-');
            }
            let mut value = 0usize;
            for bit in 0..5 {
                let offset = i * 5 + bit;
                value = (value << 1) | ((bytes[offset / 8] >> (7 - offset % 8)) & 1) as usize;
            }
            text.push(ALPHABET[value] as char);
        }
        let (digest, _) = submission_digest(&text);
        (
            Self {
                digest,
                issued_at: now,
            },
            text,
        )
    }

    pub fn live(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.issued_at) < LIFETIME
    }

    /// The fake host can advance this timestamp without a real day's wait.
    pub fn expire(&mut self) {
        self.issued_at -= LIFETIME;
    }
}

fn submission_digest(text: &str) -> ([u8; 32], bool) {
    let normalized: Vec<u8> = text
        .trim()
        .bytes()
        .filter(|b| !matches!(b, b'-' | b' '))
        .map(|b| match b.to_ascii_uppercase() {
            b'O' => b'0',
            b'I' | b'L' => b'1',
            b => b,
        })
        .collect();
    let valid = normalized.len() == 16 && normalized.iter().all(|b| ALPHABET.contains(b));
    (Sha256::digest(&normalized).into(), valid)
}

/// Hash every submission before selecting a live or dummy digest; compare all 32 bytes.
pub fn matches(code: Option<&SetupCode>, text: &str, now: Instant) -> bool {
    let (given, shape_valid) = submission_digest(text);
    let live = code.filter(|c| c.live(now));
    let expected = live.map(|c| c.digest).unwrap_or([0; 32]);
    let mut difference = u8::from(!shape_valid) | u8::from(live.is_none());
    for (a, b) in given.iter().zip(expected.iter()) {
        difference |= a ^ b;
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_format_entropy_and_digest_only() {
        let now = Instant::now();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..10_000 {
            let (code, text) = SetupCode::issue(now);
            assert_eq!(text.len(), 19);
            assert!(text
                .split('-')
                .all(|group| group.len() == 4 && group.bytes().all(|b| ALPHABET.contains(&b))));
            assert!(seen.insert(text.clone()));
            assert!(matches(Some(&code), &text, now));
            let SetupCode { digest, issued_at } = code;
            assert_eq!(digest.len(), 32);
            assert_eq!(issued_at, now);
        }
    }

    #[test]
    fn normalization_and_dummy_comparison() {
        let now = Instant::now();
        let (digest, _) = submission_digest("0101-ABCDEFGHJK-MN");
        let code = SetupCode {
            digest,
            issued_at: now,
        };
        for text in [
            "0101ABCDEFGHJKMN",
            " olol-abcdefghjk-mn ",
            "O1O1 ABCDEFGHJKMN",
        ] {
            assert!(matches(Some(&code), text, now), "{text}");
        }
        for text in [
            "",
            "0101ABCDEFGHJKM",
            "0101ABCDEFGHJKMNU",
            "0101ABCDEFGHJKMＮ",
        ] {
            assert!(!matches(Some(&code), text, now), "{text}");
            assert!(!matches(None, text, now));
        }
        assert!(!matches(None, "0101ABCDEFGHJKMN", now));
    }

    #[test]
    fn expiry_boundary() {
        let now = Instant::now();
        let (code, text) = SetupCode::issue(now);
        assert!(matches(
            Some(&code),
            &text,
            now + LIFETIME - Duration::from_secs(60)
        ));
        assert!(!matches(Some(&code), &text, now + LIFETIME));
        assert!(!matches(
            Some(&code),
            &text,
            now + LIFETIME + Duration::from_secs(1)
        ));
    }
}
