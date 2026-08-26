//! IFC schema versions, and detecting one from a file header.
//!
//! Reading the header is worth doing natively even though parsing is delegated: it decides
//! which backend can handle a file, and it is the difference between "unsupported schema" and
//! a confusing failure three layers down.

use crate::IfcError;
use serde::{Deserialize, Serialize};
use std::fmt;

/// An IFC schema version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum IfcSchema {
    Ifc2x3,
    Ifc4,
    Ifc4x3,
    /// IFC5 / IFCX. **Alpha** as of 2026: component-based and layered, borrowing workflows
    /// from the USD ecosystem, with the schema defined in TypeSpec. Supported as a serialiser
    /// target, never as the internal model (`docs/research/LANDSCAPE.md` §4).
    Ifc5,
}

impl IfcSchema {
    /// The identifier as it appears in a STEP `FILE_SCHEMA` header.
    pub fn header_name(self) -> &'static str {
        match self {
            Self::Ifc2x3 => "IFC2X3",
            Self::Ifc4 => "IFC4",
            Self::Ifc4x3 => "IFC4X3",
            Self::Ifc5 => "IFC5",
        }
    }

    /// Whether this schema is serialised as STEP physical file rather than as IFCX.
    pub fn is_step(self) -> bool {
        !matches!(self, Self::Ifc5)
    }

    /// Conventional file extension.
    pub fn extension(self) -> &'static str {
        if self.is_step() {
            "ifc"
        } else {
            "ifcx"
        }
    }

    /// Match a schema identifier, tolerating the real-world variants exporters emit —
    /// `IFC4X3_ADD2`, `IFC2X3_TC1`, and so on.
    pub fn from_identifier(raw: &str) -> Option<Self> {
        // Strip the STEP list syntax as well as quotes: a token arrives looking like
        // `('IFC4'` once `FILE_SCHEMA(('IFC4'))` has been split on commas.
        let upper = raw
            .trim_matches(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '\'' | '"'))
            .to_ascii_uppercase();
        // Longest prefix first: IFC4X3 must be tested before IFC4, or every IFC4X3 file is
        // silently mis-detected as IFC4.
        [Self::Ifc4x3, Self::Ifc2x3, Self::Ifc5, Self::Ifc4]
            .into_iter()
            .find(|candidate| upper.starts_with(candidate.header_name()))
    }

    /// Detect the schema from the head of an IFC file.
    ///
    /// Only the header is inspected, so this stays cheap on a 500 MB file — pass the first
    /// few kilobytes.
    pub fn detect(header: &str) -> Result<Self, IfcError> {
        let upper = header.to_ascii_uppercase();
        let start = upper.find("FILE_SCHEMA").ok_or(IfcError::UnknownSchema)?;
        let open = upper[start..].find('(').ok_or(IfcError::UnknownSchema)? + start;
        let close = upper[open..].find(')').ok_or(IfcError::UnknownSchema)? + open;

        upper[open + 1..close]
            .split(',')
            .find_map(Self::from_identifier)
            .ok_or(IfcError::UnknownSchema)
    }
}

impl fmt::Display for IfcSchema {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.header_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "ISO-10303-21;\n\
HEADER;\n\
FILE_DESCRIPTION(('ViewDefinition [CoordinationView]'),'2;1');\n\
FILE_NAME('example.ifc','2026-08-25T09:00:00',(''),(''),'','','');\n\
FILE_SCHEMA(('IFC4'));\n\
ENDSEC;\n";

    #[test]
    fn detects_a_normal_header() {
        assert_eq!(IfcSchema::detect(HEADER).unwrap(), IfcSchema::Ifc4);
    }

    #[test]
    fn ifc4x3_is_not_mistaken_for_ifc4() {
        // The prefix trap: a naive `starts_with("IFC4")` silently mis-detects every
        // infrastructure model.
        let header = HEADER.replace("IFC4", "IFC4X3_ADD2");
        assert_eq!(IfcSchema::detect(&header).unwrap(), IfcSchema::Ifc4x3);
    }

    #[test]
    fn real_world_schema_variants_are_tolerated() {
        assert_eq!(
            IfcSchema::from_identifier("'IFC2X3_TC1'"),
            Some(IfcSchema::Ifc2x3)
        );
        assert_eq!(IfcSchema::from_identifier(" ifc4 "), Some(IfcSchema::Ifc4));
        assert_eq!(
            IfcSchema::from_identifier("IFC4X3_ADD2"),
            Some(IfcSchema::Ifc4x3)
        );
        assert_eq!(IfcSchema::from_identifier("STEP"), None);
    }

    #[test]
    fn a_missing_or_broken_header_is_an_error_not_a_guess() {
        assert_eq!(
            IfcSchema::detect("not an ifc file"),
            Err(IfcError::UnknownSchema)
        );
        assert_eq!(
            IfcSchema::detect("FILE_SCHEMA(('SOMETHING_ELSE'));"),
            Err(IfcError::UnknownSchema)
        );
        // Truncated mid-header — a real failure mode for a streamed upload.
        assert_eq!(
            IfcSchema::detect("FILE_SCHEMA(('IFC4'"),
            Err(IfcError::UnknownSchema)
        );
    }

    #[test]
    fn detection_is_case_insensitive() {
        assert_eq!(
            IfcSchema::detect("file_schema(('ifc4'));").unwrap(),
            IfcSchema::Ifc4
        );
    }

    #[test]
    fn ifc5_serialises_as_ifcx_not_step() {
        assert!(IfcSchema::Ifc4.is_step());
        assert_eq!(IfcSchema::Ifc4.extension(), "ifc");
        assert!(!IfcSchema::Ifc5.is_step());
        assert_eq!(IfcSchema::Ifc5.extension(), "ifcx");
    }
}
