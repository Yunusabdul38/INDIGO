//! Seed handling and per-rail key derivation.
//!
//! TEMPORARY (Phases 1-4): the seed comes from the `INDIGO_TEST_MNEMONIC`
//! environment variable and every rail uses the same BIP39 seed. Phase 5
//! replaces this with secure storage and one BIP85 child secret per rail.

use indigo_domain::{Error, Result};

pub const TEST_MNEMONIC_ENV: &str = "INDIGO_TEST_MNEMONIC";

/// A wallet seed. Never printed, logged or serialised.
pub struct Seed {
    words: String,
    bytes: [u8; 64],
}

impl Seed {
    pub fn from_mnemonic(words: &str) -> Result<Self> {
        let mnemonic = bip39::Mnemonic::parse_normalized(words.trim())
            .map_err(|e| Error::InvalidInput(format!("not a valid 12/24-word phrase: {e}")))?;
        Ok(Self { words: mnemonic.to_string(), bytes: mnemonic.to_seed("") })
    }

    /// A new random 12-word phrase (test wallets; Phase 5 adds real backup).
    pub fn generate_words() -> String {
        bip39::Mnemonic::generate(12).map(|m| m.to_string()).unwrap_or_default()
    }

    /// Test seed from `INDIGO_TEST_MNEMONIC`.
    pub fn from_test_env() -> Result<Self> {
        let words = std::env::var(TEST_MNEMONIC_ENV).map_err(|_| {
            Error::InvalidInput(format!(
                "set {TEST_MNEMONIC_ENV} to a test-only 12-word phrase (Phase 5 replaces this with real key handling)"
            ))
        })?;
        Self::from_mnemonic(&words)
    }

    /// The secret for the Cashu rail. Phase 5: a BIP85 child, not the root seed.
    pub fn cashu(&self) -> [u8; 64] {
        self.bytes
    }

    /// The secret for the Ark rails. Phase 5: a BIP85 child, not the root seed.
    pub fn ark(&self) -> [u8; 64] {
        self.bytes
    }

    /// The phrase for the Fedimint rail: fedimint-sdk derives its own keys
    /// from words. Phase 5: a BIP85 child phrase, not the root one.
    pub fn fedimint_words(&self) -> &str {
        &self.words
    }
}

impl std::fmt::Debug for Seed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Seed(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_words_and_never_prints_the_seed() {
        assert!(Seed::from_mnemonic("not a real phrase").is_err());
        let seed = Seed::from_mnemonic(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .unwrap();
        assert_eq!(format!("{seed:?}"), "Seed(<redacted>)");
    }
}
