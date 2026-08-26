//! Property sets.
//!
//! Deliberately `BTreeMap`-backed rather than `HashMap`: export must be byte-reproducible for
//! a given revision, and golden-file tests depend on stable ordering
//! (`docs/ifc-semantics.md` §7.2, §12.2).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A single property value.
///
/// The measure variants carry their IFC measure type because a length and a bare real export
/// differently and compare differently. Collapsing them to `f64` loses information the
/// exporter needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PropertyValue {
    Text(String),
    Integer(i64),
    Real(f64),
    Boolean(bool),
    /// `IfcLengthMeasure`, in metres. CADForge is metric internally; display units are a UI
    /// concern (`docs/ifc-semantics.md` §11 Phase 0, units policy).
    Length(f64),
    /// `IfcAreaMeasure`, in square metres.
    Area(f64),
    /// `IfcVolumeMeasure`, in cubic metres.
    Volume(f64),
    /// `IfcCountMeasure`.
    Count(i64),
}

impl PropertyValue {
    /// The IFC measure type name, for export.
    pub fn ifc_type(&self) -> &'static str {
        match self {
            Self::Text(_) => "IfcText",
            Self::Integer(_) => "IfcInteger",
            Self::Real(_) => "IfcReal",
            Self::Boolean(_) => "IfcBoolean",
            Self::Length(_) => "IfcLengthMeasure",
            Self::Area(_) => "IfcAreaMeasure",
            Self::Volume(_) => "IfcVolumeMeasure",
            Self::Count(_) => "IfcCountMeasure",
        }
    }

    /// Numeric view, where one exists. `None` for text and boolean.
    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Self::Integer(v) | Self::Count(v) => Some(v as f64),
            Self::Real(v) | Self::Length(v) | Self::Area(v) | Self::Volume(v) => Some(v),
            Self::Text(_) | Self::Boolean(_) => None,
        }
    }
}

/// One `IfcPropertySet`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PropertySet {
    pub properties: BTreeMap<String, PropertyValue>,
}

/// All property sets on an element, keyed by set name (`Pset_WallCommon`, and so on).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PropertySets {
    sets: BTreeMap<String, PropertySet>,
}

impl PropertySets {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, set: &str, name: &str) -> Option<&PropertyValue> {
        self.sets.get(set)?.properties.get(name)
    }

    /// Set or clear a property. `None` removes it, and removes the set once it is empty so
    /// that set-then-unset leaves no trace — which is what makes command inversion exact.
    ///
    /// Returns the previous value, which is what the inverse command is built from.
    pub fn set(
        &mut self,
        set: &str,
        name: &str,
        value: Option<PropertyValue>,
    ) -> Option<PropertyValue> {
        match value {
            Some(v) => self
                .sets
                .entry(set.to_owned())
                .or_default()
                .properties
                .insert(name.to_owned(), v),
            None => {
                let entry = self.sets.get_mut(set)?;
                let previous = entry.properties.remove(name);
                if entry.properties.is_empty() {
                    self.sets.remove(set);
                }
                previous
            }
        }
    }

    pub fn set_names(&self) -> impl Iterator<Item = &str> {
        self.sets.keys().map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &PropertySet)> {
        self.sets.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }

    pub fn len(&self) -> usize {
        self.sets.values().map(|s| s.properties.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_then_unset_leaves_no_trace() {
        let mut p = PropertySets::new();
        assert!(p
            .set(
                "Pset_WallCommon",
                "IsExternal",
                Some(PropertyValue::Boolean(true))
            )
            .is_none());
        assert_eq!(
            p.get("Pset_WallCommon", "IsExternal"),
            Some(&PropertyValue::Boolean(true))
        );

        let previous = p.set("Pset_WallCommon", "IsExternal", None);
        assert_eq!(previous, Some(PropertyValue::Boolean(true)));
        // The empty set is dropped too, so this equals a freshly constructed value.
        assert_eq!(p, PropertySets::new());
    }

    #[test]
    fn set_returns_the_previous_value_for_inversion() {
        let mut p = PropertySets::new();
        p.set(
            "Pset_WallCommon",
            "FireRating",
            Some(PropertyValue::Text("60".into())),
        );
        let previous = p.set(
            "Pset_WallCommon",
            "FireRating",
            Some(PropertyValue::Text("90".into())),
        );
        assert_eq!(previous, Some(PropertyValue::Text("60".into())));
    }

    #[test]
    fn measures_keep_their_ifc_type() {
        assert_eq!(PropertyValue::Length(2.4).ifc_type(), "IfcLengthMeasure");
        assert_eq!(PropertyValue::Real(2.4).ifc_type(), "IfcReal");
        // Same number, different meaning — which is exactly why they are distinct variants.
        assert_ne!(PropertyValue::Length(2.4), PropertyValue::Real(2.4));
        assert_eq!(PropertyValue::Length(2.4).as_f64(), Some(2.4));
        assert_eq!(PropertyValue::Text("x".into()).as_f64(), None);
    }

    #[test]
    fn ordering_is_stable_for_reproducible_export() {
        let mut a = PropertySets::new();
        a.set("B", "two", Some(PropertyValue::Integer(2)));
        a.set("A", "one", Some(PropertyValue::Integer(1)));

        let mut b = PropertySets::new();
        b.set("A", "one", Some(PropertyValue::Integer(1)));
        b.set("B", "two", Some(PropertyValue::Integer(2)));

        assert_eq!(a.set_names().collect::<Vec<_>>(), ["A", "B"]);
        assert_eq!(a, b, "insertion order must not affect the model");
    }
}
