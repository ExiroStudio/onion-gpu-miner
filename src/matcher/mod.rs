//! Matcher layer — prefix comparison against the derived onion hosts.
//!
//! Supports multiple target prefixes. Stateless and read-only, so it can be
//! shared across threads behind an `Arc` with no synchronization.

use crate::types::DerivedAddress;

/// Holds the set of target prefixes and tests derived addresses against them.
pub struct Matcher {
    prefixes: Vec<String>,
}

impl Matcher {
    pub fn new(prefixes: Vec<String>) -> Self {
        Self { prefixes }
    }

    pub fn prefixes(&self) -> &[String] {
        &self.prefixes
    }

    /// Return the first matching prefix for an onion host, if any.
    #[inline]
    pub fn match_one(&self, onion: &str) -> Option<&str> {
        self.prefixes
            .iter()
            .find(|p| onion.starts_with(p.as_str()))
            .map(|s| s.as_str())
    }

    /// Scan a whole batch and return `(index_into_batch, matched_prefix)` for
    /// every hit. Almost always empty, so this stays cheap.
    pub fn scan(&self, batch: &[DerivedAddress]) -> Vec<(usize, String)> {
        let mut hits = Vec::new();
        for (i, addr) in batch.iter().enumerate() {
            if let Some(p) = self.match_one(&addr.onion) {
                hits.push((i, p.to_string()));
            }
        }
        hits
    }
}
