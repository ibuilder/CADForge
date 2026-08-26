//! The swappable backend.

use crate::schema::IfcSchema;
use crate::IfcError;
use cadforge_core::{GlobalId, Model};
use serde::{Deserialize, Serialize};

/// What a backend can do.
///
/// Declared rather than assumed, so the UI can grey out "Export IFC4X3" instead of offering
/// it and failing, and so the desktop build can escalate to a heavier backend only when the
/// lighter one cannot handle a file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackendCapabilities {
    pub reads: Vec<IfcSchema>,
    pub writes: Vec<IfcSchema>,
    /// Whether the backend can produce geometry, or only semantics.
    pub geometry: bool,
    /// Whether it runs on mobile. An IfcOpenShell subprocess does not (ADR-0006).
    pub mobile: bool,
}

impl BackendCapabilities {
    pub fn none() -> Self {
        Self {
            reads: Vec::new(),
            writes: Vec::new(),
            geometry: false,
            mobile: false,
        }
    }

    pub fn can_read(&self, schema: IfcSchema) -> bool {
        self.reads.contains(&schema)
    }

    pub fn can_write(&self, schema: IfcSchema) -> bool {
        self.writes.contains(&schema)
    }
}

/// Something that went wrong with a file but did not stop the import.
///
/// Imported IFC is untrusted input and real-world files are full of problems that must not
/// abort a load (`docs/ifc-semantics.md` §13). Every one is surfaced in the import report rather
/// than logged and forgotten.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ImportWarning {
    /// Two entities claimed the same `GlobalId`. Common in files assembled from other files.
    DuplicateGlobalId(GlobalId),
    /// An identity that is not a valid `IfcGloballyUniqueId`; a fresh one was minted.
    InvalidGlobalId { raw: String, replacement: GlobalId },
    /// Geometry could not be produced for an element. It keeps its semantics and loses its
    /// representation — never the other way around.
    GeometryFailed { element: GlobalId, reason: String },
    /// A relationship pointed at something absent from the file.
    DanglingReference {
        element: GlobalId,
        relationship: String,
    },
    /// An entity CADForge does not model natively. Preserved as `IfcClass::Other`.
    UnsupportedEntity { entity: String, count: usize },
}

/// The outcome of reading a file.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportReport {
    pub schema: IfcSchema,
    pub elements: usize,
    pub warnings: Vec<ImportWarning>,
    /// Elements whose geometry could not be produced.
    pub geometry_failures: usize,
}

impl ImportReport {
    pub fn is_clean(&self) -> bool {
        self.warnings.is_empty()
    }
}

/// An IFC reader/writer.
///
/// Implementations own their own types entirely; only `cadforge-core` types cross this
/// boundary.
pub trait IfcBackend {
    fn name(&self) -> &'static str;

    fn capabilities(&self) -> BackendCapabilities;

    /// Read a file into the model, appending to whatever is already there.
    fn read(&self, bytes: &[u8], model: &mut Model) -> Result<ImportReport, IfcError>;

    /// Serialise the model.
    fn write(&self, model: &Model, schema: IfcSchema) -> Result<Vec<u8>, IfcError>;

    /// Default guard so implementations do not each re-derive it.
    fn check_can_write(&self, schema: IfcSchema) -> Result<(), IfcError> {
        let caps = self.capabilities();
        if caps.writes.is_empty() {
            return Err(IfcError::ReadOnlyBackend {
                backend: self.name(),
            });
        }
        if !caps.can_write(schema) {
            return Err(IfcError::UnsupportedSchema {
                backend: self.name(),
                schema,
            });
        }
        Ok(())
    }
}

/// The registered default: refuses everything.
///
/// Present so the boundary is exercised and every caller handles the failure path from day
/// one, rather than discovering it when the first real backend arrives.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnimplementedBackend;

impl IfcBackend for UnimplementedBackend {
    fn name(&self) -> &'static str {
        "unimplemented"
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::none()
    }

    fn read(&self, _bytes: &[u8], _model: &mut Model) -> Result<ImportReport, IfcError> {
        Err(IfcError::NoBackend)
    }

    fn write(&self, _model: &Model, _schema: IfcSchema) -> Result<Vec<u8>, IfcError> {
        Err(IfcError::NoBackend)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in that reads IFC4 and writes nothing — the shape a viewer-only backend takes.
    struct ReadOnly;

    impl IfcBackend for ReadOnly {
        fn name(&self) -> &'static str {
            "read-only"
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                reads: vec![IfcSchema::Ifc2x3, IfcSchema::Ifc4],
                writes: Vec::new(),
                geometry: true,
                mobile: true,
            }
        }

        fn read(&self, _bytes: &[u8], _model: &mut Model) -> Result<ImportReport, IfcError> {
            Ok(ImportReport {
                schema: IfcSchema::Ifc4,
                elements: 0,
                warnings: Vec::new(),
                geometry_failures: 0,
            })
        }

        fn write(&self, _model: &Model, schema: IfcSchema) -> Result<Vec<u8>, IfcError> {
            self.check_can_write(schema)?;
            unreachable!("the guard above always rejects")
        }
    }

    #[test]
    fn the_default_backend_refuses_rather_than_pretending() {
        let mut model = Model::new();
        assert_eq!(
            UnimplementedBackend.read(b"", &mut model),
            Err(IfcError::NoBackend)
        );
        assert_eq!(
            UnimplementedBackend.write(&model, IfcSchema::Ifc4),
            Err(IfcError::NoBackend)
        );
        assert!(
            model.is_empty(),
            "a failed read must not half-populate the model"
        );
    }

    #[test]
    fn a_read_only_backend_reports_itself_as_read_only() {
        let backend = ReadOnly;
        assert_eq!(
            backend.write(&Model::new(), IfcSchema::Ifc4),
            Err(IfcError::ReadOnlyBackend {
                backend: "read-only"
            })
        );
    }

    #[test]
    fn capabilities_are_queryable_before_an_operation_is_offered() {
        let caps = ReadOnly.capabilities();
        assert!(caps.can_read(IfcSchema::Ifc4));
        assert!(!caps.can_read(IfcSchema::Ifc5));
        assert!(!caps.can_write(IfcSchema::Ifc4));
        assert!(caps.mobile);
    }

    #[test]
    fn an_import_report_distinguishes_clean_from_survivable() {
        let clean = ImportReport {
            schema: IfcSchema::Ifc4,
            elements: 12,
            warnings: Vec::new(),
            geometry_failures: 0,
        };
        assert!(clean.is_clean());

        let messy = ImportReport {
            schema: IfcSchema::Ifc2x3,
            elements: 12,
            warnings: vec![ImportWarning::DuplicateGlobalId(GlobalId::new())],
            geometry_failures: 1,
        };
        assert!(
            !messy.is_clean(),
            "a duplicate GlobalId must not pass silently"
        );
    }
}
