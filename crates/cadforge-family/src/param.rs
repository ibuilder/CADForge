//! Family parameters.
//!
//! Modelled on what Revit and GDL both converge on: typed parameters with defaults, scoped to
//! either the type or the instance. A door's `Width` is type-scoped — every "900 × 2100" door
//! is 900 wide, and changing it changes them all. Its `Sill Height` is instance-scoped.
//!
//! Getting that scoping wrong is the classic family-authoring bug, so it is enforced here
//! rather than left to convention.

use crate::FamilyError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// What a parameter holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParamType {
    /// Metres. CADForge is metric internally; display units are a UI concern.
    Length,
    /// Radians.
    Angle,
    Count,
    Number,
    Text,
    Boolean,
    /// A material identity, resolved against the material library.
    Material,
}

impl ParamType {
    pub fn name(self) -> &'static str {
        match self {
            Self::Length => "Length",
            Self::Angle => "Angle",
            Self::Count => "Count",
            Self::Number => "Number",
            Self::Text => "Text",
            Self::Boolean => "Boolean",
            Self::Material => "Material",
        }
    }

    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            Self::Length | Self::Angle | Self::Count | Self::Number
        )
    }
}

/// A parameter value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ParamValue {
    Length(f64),
    Angle(f64),
    Count(i64),
    Number(f64),
    Text(String),
    Boolean(bool),
    Material(String),
}

impl ParamValue {
    pub fn param_type(&self) -> ParamType {
        match self {
            Self::Length(_) => ParamType::Length,
            Self::Angle(_) => ParamType::Angle,
            Self::Count(_) => ParamType::Count,
            Self::Number(_) => ParamType::Number,
            Self::Text(_) => ParamType::Text,
            Self::Boolean(_) => ParamType::Boolean,
            Self::Material(_) => ParamType::Material,
        }
    }

    /// Numeric view, for expression evaluation. `None` for text, boolean, and material.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Length(v) | Self::Angle(v) | Self::Number(v) => Some(*v),
            Self::Count(v) => Some(*v as f64),
            Self::Text(_) | Self::Boolean(_) | Self::Material(_) => None,
        }
    }
}

/// Whether a parameter varies per instance or is fixed by the type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParamScope {
    /// Fixed by the family type. Changing it changes every instance of that type.
    Type,
    /// Varies per placed instance.
    Instance,
}

/// A parameter declaration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamDef {
    pub name: String,
    pub param_type: ParamType,
    pub default: ParamValue,
    pub scope: ParamScope,
    /// Inclusive numeric bounds. Ignored for non-numeric types.
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub description: Option<String>,
}

impl ParamDef {
    pub fn new(name: impl Into<String>, default: ParamValue, scope: ParamScope) -> Self {
        Self {
            name: name.into(),
            param_type: default.param_type(),
            default,
            scope,
            min: None,
            max: None,
            description: None,
        }
    }

    /// A type-scoped length — the most common declaration by far.
    pub fn length(name: impl Into<String>, default: f64) -> Self {
        Self::new(name, ParamValue::Length(default), ParamScope::Type)
    }

    /// An instance-scoped length.
    pub fn instance_length(name: impl Into<String>, default: f64) -> Self {
        Self::new(name, ParamValue::Length(default), ParamScope::Instance)
    }

    pub fn with_range(mut self, min: f64, max: f64) -> Self {
        self.min = Some(min);
        self.max = Some(max);
        self
    }

    pub fn described(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Check a candidate value against this declaration.
    pub fn validate(&self, value: &ParamValue) -> Result<(), FamilyError> {
        if value.param_type() != self.param_type {
            return Err(FamilyError::TypeMismatch {
                name: self.name.clone(),
                expected: self.param_type.name(),
                actual: value.param_type().name(),
            });
        }
        if let Some(v) = value.as_f64() {
            let min = self.min.unwrap_or(f64::NEG_INFINITY);
            let max = self.max.unwrap_or(f64::INFINITY);
            if !v.is_finite() || v < min || v > max {
                return Err(FamilyError::OutOfRange {
                    name: self.name.clone(),
                    value: v,
                    min,
                    max,
                });
            }
        }
        Ok(())
    }
}

/// A fully resolved set of parameter values: defaults, then type overrides, then instance
/// overrides.
///
/// `BTreeMap`-backed so that iteration — and therefore export and hashing — is stable.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ParamBag {
    values: BTreeMap<String, ParamValue>,
}

impl ParamBag {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, name: impl Into<String>, value: ParamValue) {
        self.values.insert(name.into(), value);
    }

    pub fn get(&self, name: &str) -> Option<&ParamValue> {
        self.values.get(name)
    }

    /// Numeric lookup for expression evaluation.
    ///
    /// Distinguishes "no such parameter" from "that parameter is text", because those are
    /// different authoring mistakes and deserve different messages.
    pub fn number(&self, name: &str) -> Result<f64, FamilyError> {
        match self.values.get(name) {
            None => Err(FamilyError::UnknownParameter(name.to_owned())),
            Some(v) => v
                .as_f64()
                .ok_or_else(|| FamilyError::NotNumeric(name.to_owned())),
        }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &ParamValue)> {
        self.values.iter().map(|(k, v)| (k.as_str(), v))
    }
}

impl FromIterator<(String, ParamValue)> for ParamBag {
    fn from_iter<T: IntoIterator<Item = (String, ParamValue)>>(iter: T) -> Self {
        Self {
            values: iter.into_iter().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_is_inferred_from_the_default() {
        let p = ParamDef::length("Width", 0.9);
        assert_eq!(p.param_type, ParamType::Length);
        assert_eq!(p.scope, ParamScope::Type);
    }

    #[test]
    fn a_mismatched_type_is_rejected() {
        let p = ParamDef::length("Width", 0.9);
        let err = p.validate(&ParamValue::Text("wide".into())).unwrap_err();
        assert_eq!(
            err,
            FamilyError::TypeMismatch {
                name: "Width".into(),
                expected: "Length",
                actual: "Text",
            }
        );
    }

    #[test]
    fn range_bounds_are_inclusive_and_reject_non_finite() {
        let p = ParamDef::length("Width", 0.9).with_range(0.4, 2.0);
        assert!(p.validate(&ParamValue::Length(0.4)).is_ok());
        assert!(p.validate(&ParamValue::Length(2.0)).is_ok());
        assert!(p.validate(&ParamValue::Length(2.001)).is_err());
        assert!(p.validate(&ParamValue::Length(f64::NAN)).is_err());
        assert!(p.validate(&ParamValue::Length(f64::INFINITY)).is_err());
    }

    #[test]
    fn an_unbounded_parameter_still_rejects_nan() {
        let p = ParamDef::length("Width", 0.9);
        assert!(p.validate(&ParamValue::Length(1e9)).is_ok());
        assert!(p.validate(&ParamValue::Length(f64::NAN)).is_err());
    }

    #[test]
    fn the_bag_distinguishes_missing_from_non_numeric() {
        let mut bag = ParamBag::new();
        bag.insert("Width", ParamValue::Length(0.9));
        bag.insert("Finish", ParamValue::Text("oak".into()));

        assert_eq!(bag.number("Width").unwrap(), 0.9);
        assert_eq!(
            bag.number("Finish"),
            Err(FamilyError::NotNumeric("Finish".into()))
        );
        assert_eq!(
            bag.number("Height"),
            Err(FamilyError::UnknownParameter("Height".into()))
        );
    }

    #[test]
    fn counts_read_as_numbers() {
        assert_eq!(ParamValue::Count(3).as_f64(), Some(3.0));
        assert_eq!(ParamValue::Boolean(true).as_f64(), None);
        assert!(ParamType::Length.is_numeric());
        assert!(!ParamType::Material.is_numeric());
    }
}
