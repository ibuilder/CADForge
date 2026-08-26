//! IFC identity.
//!
//! `GlobalId` is an `IfcGloballyUniqueId`: a 128-bit UUID compressed into 22 characters of
//! IFC's own base-64 alphabet. It is the only cross-system identity CADForge recognises.
//! Display names are never keys (`docs/ifc-semantics.md` §4.1).

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// IFC's base-64 alphabet. Note it is *not* RFC 4648 — the last two symbols are `_` and `$`,
/// and the ordering is digits, uppercase, lowercase.
const CHARS: &[u8; 64] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz_$";

/// Length of an `IfcGloballyUniqueId` in characters.
pub const GLOBAL_ID_LEN: usize = 22;

/// A stable, IFC-compatible element identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GlobalId(String);

/// Reasons a string is not a valid `IfcGloballyUniqueId`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GlobalIdError {
    #[error("expected {GLOBAL_ID_LEN} characters, got {0}")]
    WrongLength(usize),
    #[error("character {0:?} is not in the IFC base-64 alphabet")]
    InvalidChar(char),
    /// The leading character encodes the high bits of byte 0, so it can only be `0`–`3`.
    #[error("leading character {0:?} would overflow 128 bits")]
    Overflow(char),
}

impl GlobalId {
    /// Mint a fresh identity from a random UUID.
    pub fn new() -> Self {
        Self::from_uuid(Uuid::new_v4())
    }

    /// Compress a UUID into IFC's 22-character form.
    pub fn from_uuid(uuid: Uuid) -> Self {
        let bytes = uuid.as_bytes();
        let mut out = String::with_capacity(GLOBAL_ID_LEN);

        // Byte 0 alone in two digits, then five groups of three bytes in four digits each:
        // 2 + 5 * 4 = 22 characters, 8 + 5 * 24 = 128 bits.
        push_digits(u32::from(bytes[0]), 2, &mut out);
        for chunk in bytes[1..].chunks_exact(3) {
            let v = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
            push_digits(v, 4, &mut out);
        }

        debug_assert_eq!(out.len(), GLOBAL_ID_LEN);
        GlobalId(out)
    }

    /// Build from raw bytes.
    ///
    /// Used to derive *deterministic* identities — an exporter that mints a random GlobalId
    /// for each synthesised entity produces a different file every run, which defeats
    /// content-addressed revisions and golden-file tests.
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self::from_uuid(Uuid::from_bytes(bytes))
    }

    /// Accept an identity that came from a file, validating it rather than trusting it.
    ///
    /// Imported IFC is untrusted input (`docs/ifc-semantics.md` §13).
    pub fn parse(s: &str) -> Result<Self, GlobalIdError> {
        if s.len() != GLOBAL_ID_LEN {
            return Err(GlobalIdError::WrongLength(s.len()));
        }
        for c in s.chars() {
            if digit_of(c).is_none() {
                return Err(GlobalIdError::InvalidChar(c));
            }
        }
        let lead = s.chars().next().expect("length checked above");
        if digit_of(lead).expect("validated above") > 3 {
            return Err(GlobalIdError::Overflow(lead));
        }
        Ok(GlobalId(s.to_owned()))
    }

    /// Expand back to a UUID. Round-trips with [`GlobalId::from_uuid`].
    pub fn to_uuid(&self) -> Uuid {
        let digits: Vec<u32> = self
            .0
            .chars()
            .map(|c| digit_of(c).expect("GlobalId is validated on construction"))
            .collect();

        let mut bytes = [0u8; 16];
        bytes[0] = (digits[0] * 64 + digits[1]) as u8;
        for (group, out) in digits[2..]
            .chunks_exact(4)
            .zip(bytes[1..].chunks_exact_mut(3))
        {
            let v = group.iter().fold(0u32, |acc, d| acc * 64 + d);
            out[0] = (v >> 16) as u8;
            out[1] = (v >> 8) as u8;
            out[2] = v as u8;
        }
        Uuid::from_bytes(bytes)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for GlobalId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for GlobalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn push_digits(mut v: u32, n: usize, out: &mut String) {
    let mut buf = [0u8; 4];
    for slot in buf[..n].iter_mut().rev() {
        *slot = CHARS[(v % 64) as usize];
        v /= 64;
    }
    out.push_str(std::str::from_utf8(&buf[..n]).expect("alphabet is ASCII"));
}

fn digit_of(c: char) -> Option<u32> {
    if !c.is_ascii() {
        return None;
    }
    CHARS.iter().position(|&b| b == c as u8).map(|i| i as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_uuid() {
        for _ in 0..1000 {
            let uuid = Uuid::new_v4();
            let id = GlobalId::from_uuid(uuid);
            assert_eq!(id.as_str().len(), GLOBAL_ID_LEN);
            assert_eq!(id.to_uuid(), uuid, "round trip failed for {uuid}");
        }
    }

    #[test]
    fn parses_what_it_generates() {
        let id = GlobalId::new();
        assert_eq!(GlobalId::parse(id.as_str()), Ok(id));
    }

    #[test]
    fn rejects_malformed_ids() {
        assert_eq!(
            GlobalId::parse("too-short"),
            Err(GlobalIdError::WrongLength(9))
        );
        // 22 characters, but '*' is outside the IFC alphabet.
        assert_eq!(
            GlobalId::parse("0*00000000000000000000"),
            Err(GlobalIdError::InvalidChar('*'))
        );
        // A leading digit above 3 encodes more than 128 bits.
        assert_eq!(
            GlobalId::parse("9000000000000000000000"),
            Err(GlobalIdError::Overflow('9'))
        );
    }

    #[test]
    fn derived_ids_are_stable_and_valid() {
        let a = GlobalId::from_bytes([7; 16]);
        let b = GlobalId::from_bytes([7; 16]);
        assert_eq!(a, b, "the same bytes must always give the same identity");
        assert!(GlobalId::parse(a.as_str()).is_ok());
        assert_ne!(a, GlobalId::from_bytes([8; 16]));
        // Every possible leading byte still yields a parseable id.
        for lead in [0u8, 1, 63, 64, 191, 255] {
            let mut bytes = [0u8; 16];
            bytes[0] = lead;
            let id = GlobalId::from_bytes(bytes);
            assert!(
                GlobalId::parse(id.as_str()).is_ok(),
                "leading byte {lead} failed"
            );
        }
    }

    #[test]
    fn boundary_uuids_survive() {
        for uuid in [Uuid::nil(), Uuid::from_bytes([0xff; 16])] {
            assert_eq!(GlobalId::from_uuid(uuid).to_uuid(), uuid);
        }
    }

    #[test]
    fn ids_are_unique_enough_to_key_a_model() {
        let ids: std::collections::BTreeSet<_> = (0..10_000).map(|_| GlobalId::new()).collect();
        assert_eq!(ids.len(), 10_000);
    }
}
