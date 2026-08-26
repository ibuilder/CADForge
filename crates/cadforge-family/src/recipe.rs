//! Geometry recipes.
//!
//! A recipe is a small, deterministic, acyclic list of operations. It — not the resulting
//! mesh — is the canonical geometry of an authored element (ADR-0004).
//!
//! Steps reference earlier steps by index, and a step may only reference steps *before* it.
//! That makes the graph acyclic by construction rather than by a validation pass, and it
//! makes evaluation a single forward loop.

use crate::family::RepresentationKind;
use crate::param::ParamBag;
use crate::FamilyError;
use cadforge_core::Representation;
use cadforge_geom::{extrude_along, CsgBackend, IndexedMesh, Profile, TessellationSettings};
use glam::{DVec2, DVec3};
use serde::{Deserialize, Serialize};

/// An arithmetic expression over parameters.
///
/// Deliberately small: constants, parameter references, and four operators. This covers the
/// formulas real families use (`Frame Width = Width - 2 * Jamb`) without becoming a scripting
/// language that has to be sandboxed, versioned, and made deterministic across platforms.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Expr {
    Const(f64),
    Param(String),
    Neg(Box<Expr>),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
}

impl Expr {
    pub fn param(name: impl Into<String>) -> Self {
        Self::Param(name.into())
    }

    pub fn eval(&self, params: &ParamBag) -> Result<f64, FamilyError> {
        Ok(match self {
            Self::Const(v) => *v,
            Self::Param(name) => params.number(name)?,
            Self::Neg(a) => -a.eval(params)?,
            Self::Add(a, b) => a.eval(params)? + b.eval(params)?,
            Self::Sub(a, b) => a.eval(params)? - b.eval(params)?,
            Self::Mul(a, b) => a.eval(params)? * b.eval(params)?,
            Self::Div(a, b) => {
                let divisor = b.eval(params)?;
                if divisor == 0.0 {
                    return Err(FamilyError::DivideByZero);
                }
                a.eval(params)? / divisor
            }
        })
    }

    /// Every parameter this expression reads. Used to decide which instances need re-flexing
    /// when a parameter changes.
    pub fn referenced_params(&self, out: &mut Vec<String>) {
        match self {
            Self::Const(_) => {}
            Self::Param(name) => out.push(name.clone()),
            Self::Neg(a) => a.referenced_params(out),
            Self::Add(a, b) | Self::Sub(a, b) | Self::Mul(a, b) | Self::Div(a, b) => {
                a.referenced_params(out);
                b.referenced_params(out);
            }
        }
    }
}

// Real operator traits rather than `Expr::add`-style constructors, so a family formula reads
// the way it is written on paper: `Expr::param("Width") - Expr::from(2.0) * jamb`.
impl std::ops::Add for Expr {
    type Output = Expr;
    fn add(self, rhs: Expr) -> Expr {
        Expr::Add(Box::new(self), Box::new(rhs))
    }
}

impl std::ops::Sub for Expr {
    type Output = Expr;
    fn sub(self, rhs: Expr) -> Expr {
        Expr::Sub(Box::new(self), Box::new(rhs))
    }
}

impl std::ops::Mul for Expr {
    type Output = Expr;
    fn mul(self, rhs: Expr) -> Expr {
        Expr::Mul(Box::new(self), Box::new(rhs))
    }
}

impl std::ops::Div for Expr {
    type Output = Expr;
    fn div(self, rhs: Expr) -> Expr {
        Expr::Div(Box::new(self), Box::new(rhs))
    }
}

impl std::ops::Neg for Expr {
    type Output = Expr;
    fn neg(self) -> Expr {
        Expr::Neg(Box::new(self))
    }
}

impl From<f64> for Expr {
    fn from(v: f64) -> Self {
        Expr::Const(v)
    }
}

/// How to build the 2D profile a sweep starts from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProfileSpec {
    /// Centred on the local origin.
    Rectangle {
        width: Expr,
        depth: Expr,
    },
    Circle {
        radius: Expr,
    },
    /// A literal closed polygon. Wound automatically.
    Polygon {
        points: Vec<[f64; 2]>,
    },
}

impl ProfileSpec {
    pub fn build(
        &self,
        params: &ParamBag,
        tess: &TessellationSettings,
    ) -> Result<Profile, FamilyError> {
        Ok(match self {
            Self::Rectangle { width, depth } => {
                Profile::rectangle(width.eval(params)?, depth.eval(params)?)?
            }
            Self::Circle { radius } => Profile::circle(radius.eval(params)?, tess)?,
            Self::Polygon { points } => {
                Profile::new(points.iter().map(|p| DVec2::new(p[0], p[1])))?
            }
        })
    }
}

/// One step of a recipe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RecipeOp {
    /// Sweep a profile. Maps directly onto `IfcExtrudedAreaSolid`.
    Extrude {
        profile: ProfileSpec,
        /// Sweep direction in the family's local frame.
        direction: [f64; 3],
        depth: Expr,
    },
    /// Move an earlier step.
    Translate { source: usize, offset: [Expr; 3] },
    /// Requires a CSG backend.
    Union { a: usize, b: usize },
    /// Requires a CSG backend.
    Difference { a: usize, b: usize },
}

impl RecipeOp {
    fn references(&self) -> Vec<usize> {
        match self {
            Self::Extrude { .. } => Vec::new(),
            Self::Translate { source, .. } => vec![*source],
            Self::Union { a, b } | Self::Difference { a, b } => vec![*a, *b],
        }
    }
}

/// A deterministic geometry program.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeometryRecipe {
    ops: Vec<RecipeOp>,
    output: usize,
}

impl GeometryRecipe {
    /// Build and validate. The output is the last step.
    pub fn new(ops: Vec<RecipeOp>) -> Result<Self, FamilyError> {
        let output = ops.len().checked_sub(1).ok_or(FamilyError::EmptyRecipe)?;
        Self::with_output(ops, output)
    }

    pub fn with_output(ops: Vec<RecipeOp>, output: usize) -> Result<Self, FamilyError> {
        if ops.is_empty() {
            return Err(FamilyError::EmptyRecipe);
        }
        for (index, op) in ops.iter().enumerate() {
            for referenced in op.references() {
                // Forward and self references are rejected here, which is what makes the
                // graph acyclic without a separate cycle check.
                if referenced >= index {
                    return Err(FamilyError::BadRecipeReference { index, referenced });
                }
            }
        }
        if output >= ops.len() {
            return Err(FamilyError::BadRecipeOutput(output));
        }
        Ok(Self { ops, output })
    }

    /// A single extrusion — the overwhelmingly common case.
    pub fn single_extrusion(
        profile: ProfileSpec,
        direction: [f64; 3],
        depth: Expr,
    ) -> Result<Self, FamilyError> {
        Self::new(vec![RecipeOp::Extrude {
            profile,
            direction,
            depth,
        }])
    }

    pub fn ops(&self) -> &[RecipeOp] {
        &self.ops
    }

    pub fn output(&self) -> usize {
        self.output
    }

    /// Whether this recipe exports as native parametric IFC or has to degrade.
    ///
    /// This is the capability declaration ADR-0004 requires: a single extrusion is
    /// `IfcExtrudedAreaSolid` exactly; everything else becomes a tessellated representation
    /// and is flagged as degraded in the UI and validation report.
    pub fn representation_kind(&self) -> RepresentationKind {
        match self.ops.as_slice() {
            [RecipeOp::Extrude { .. }] => RepresentationKind::NativeParametric,
            _ => RepresentationKind::Tessellated,
        }
    }

    /// Every parameter any step reads, deduplicated and sorted.
    pub fn referenced_params(&self) -> Vec<String> {
        let mut out = Vec::new();
        for op in &self.ops {
            match op {
                RecipeOp::Extrude { profile, depth, .. } => {
                    depth.referenced_params(&mut out);
                    match profile {
                        ProfileSpec::Rectangle { width, depth } => {
                            width.referenced_params(&mut out);
                            depth.referenced_params(&mut out);
                        }
                        ProfileSpec::Circle { radius } => radius.referenced_params(&mut out),
                        ProfileSpec::Polygon { .. } => {}
                    }
                }
                RecipeOp::Translate { offset, .. } => {
                    for e in offset {
                        e.referenced_params(&mut out);
                    }
                }
                RecipeOp::Union { .. } | RecipeOp::Difference { .. } => {}
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// Evaluate to an IFC-ready [`Representation`].
    ///
    /// A single extrusion short-circuits straight to `IfcExtrudedAreaSolid` **without
    /// tessellating at all** — the profile and depth go into the file, and the receiving
    /// application rebuilds the solid itself. That is what keeps a wall editable in Revit or
    /// Bonsai after a round trip.
    ///
    /// Anything else degrades to an explicit triangle set. The degradation is visible in the
    /// returned variant rather than logged, so callers cannot fail to notice it (ADR-0004).
    pub fn to_representation(
        &self,
        params: &ParamBag,
        tess: &TessellationSettings,
        csg: &dyn CsgBackend,
    ) -> Result<Representation, FamilyError> {
        if let [RecipeOp::Extrude {
            profile,
            direction,
            depth,
        }] = self.ops.as_slice()
        {
            let built = profile.build(params, tess)?;
            // IFC can carry voided profiles via IfcArbitraryProfileDefWithVoids, but writing
            // only the outer loop would silently fill the hole. Refuse instead.
            if !built.holes().is_empty() {
                return Err(FamilyError::Geometry(
                    cadforge_geom::GeometryError::HolesNotSupported(built.holes().len()),
                ));
            }
            let depth = depth.eval(params)?;
            if !depth.is_finite() || depth == 0.0 {
                return Err(FamilyError::Geometry(
                    cadforge_geom::GeometryError::InvalidDepth(depth),
                ));
            }
            let points = built.outer().iter().map(|p| [p.x, p.y]).collect();
            return Ok(Representation::extrusion(points, *direction, depth));
        }

        let mesh = self.evaluate(params, tess, csg)?;
        Ok(Representation::TriangulatedFaceSet {
            vertices: mesh.positions.iter().map(|p| [p.x, p.y, p.z]).collect(),
            faces: mesh
                .indices
                .chunks_exact(3)
                .map(|t| [t[0], t[1], t[2]])
                .collect(),
        })
    }

    /// Evaluate to a mesh.
    ///
    /// A single forward pass: step `i` can only read steps before it, so no scheduling is
    /// needed. Same inputs always produce the same output.
    pub fn evaluate(
        &self,
        params: &ParamBag,
        tess: &TessellationSettings,
        csg: &dyn CsgBackend,
    ) -> Result<IndexedMesh, FamilyError> {
        let mut results: Vec<IndexedMesh> = Vec::with_capacity(self.ops.len());

        for op in &self.ops {
            let mesh = match op {
                RecipeOp::Extrude {
                    profile,
                    direction,
                    depth,
                } => {
                    let profile = profile.build(params, tess)?;
                    let direction = DVec3::from_array(*direction);
                    extrude_along(&profile, direction, depth.eval(params)?)?
                }
                RecipeOp::Translate { source, offset } => {
                    let delta = DVec3::new(
                        offset[0].eval(params)?,
                        offset[1].eval(params)?,
                        offset[2].eval(params)?,
                    );
                    results[*source].transformed(glam::DMat4::from_translation(delta))
                }
                RecipeOp::Union { a, b } => csg.union(&results[*a], &results[*b])?,
                RecipeOp::Difference { a, b } => csg.difference(&results[*a], &results[*b])?,
            };
            results.push(mesh);
        }

        Ok(results.swap_remove(self.output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::param::ParamValue;
    use cadforge_geom::UnavailableCsg;

    fn bag() -> ParamBag {
        let mut b = ParamBag::new();
        b.insert("Width", ParamValue::Length(0.9));
        b.insert("Height", ParamValue::Length(2.1));
        b.insert("Thickness", ParamValue::Length(0.05));
        b.insert("Finish", ParamValue::Text("oak".into()));
        b
    }

    #[test]
    fn expressions_evaluate_against_parameters() {
        let params = bag();
        // Width - 2 * Thickness
        let e = Expr::param("Width") - Expr::from(2.0) * Expr::param("Thickness");
        assert!((e.eval(&params).unwrap() - 0.8).abs() < 1e-12);
    }

    #[test]
    fn expression_failures_name_the_cause() {
        let params = bag();
        assert_eq!(
            Expr::param("Missing").eval(&params),
            Err(FamilyError::UnknownParameter("Missing".into()))
        );
        assert_eq!(
            Expr::param("Finish").eval(&params),
            Err(FamilyError::NotNumeric("Finish".into()))
        );
        assert_eq!(
            (Expr::param("Width") / Expr::from(0.0)).eval(&params),
            Err(FamilyError::DivideByZero)
        );
    }

    #[test]
    fn referenced_params_are_sorted_and_deduplicated() {
        let recipe = GeometryRecipe::single_extrusion(
            ProfileSpec::Rectangle {
                width: Expr::param("Width"),
                depth: Expr::param("Thickness"),
            },
            [0.0, 0.0, 1.0],
            Expr::param("Height"),
        )
        .unwrap();
        assert_eq!(recipe.referenced_params(), ["Height", "Thickness", "Width"]);
    }

    #[test]
    fn a_single_extrusion_exports_natively() {
        let recipe = GeometryRecipe::single_extrusion(
            ProfileSpec::Rectangle {
                width: Expr::param("Width"),
                depth: Expr::param("Thickness"),
            },
            [0.0, 0.0, 1.0],
            Expr::param("Height"),
        )
        .unwrap();
        assert_eq!(
            recipe.representation_kind(),
            RepresentationKind::NativeParametric
        );

        let mesh = recipe
            .evaluate(&bag(), &TessellationSettings::standard(), &UnavailableCsg)
            .unwrap();
        // 0.9 × 0.05 × 2.1
        assert!((mesh.signed_volume() - 0.0945).abs() < 1e-9);
    }

    #[test]
    fn anything_more_than_one_extrusion_declares_itself_degraded() {
        let recipe = GeometryRecipe::new(vec![
            RecipeOp::Extrude {
                profile: ProfileSpec::Rectangle {
                    width: Expr::Const(1.0),
                    depth: Expr::Const(1.0),
                },
                direction: [0.0, 0.0, 1.0],
                depth: Expr::Const(1.0),
            },
            RecipeOp::Translate {
                source: 0,
                offset: [Expr::Const(2.0), Expr::Const(0.0), Expr::Const(0.0)],
            },
        ])
        .unwrap();
        assert_eq!(
            recipe.representation_kind(),
            RepresentationKind::Tessellated
        );
    }

    #[test]
    fn translate_moves_the_referenced_step() {
        let recipe = GeometryRecipe::new(vec![
            RecipeOp::Extrude {
                profile: ProfileSpec::Rectangle {
                    width: Expr::Const(1.0),
                    depth: Expr::Const(1.0),
                },
                direction: [0.0, 0.0, 1.0],
                depth: Expr::Const(1.0),
            },
            RecipeOp::Translate {
                source: 0,
                offset: [Expr::Const(5.0), Expr::Const(0.0), Expr::Const(0.0)],
            },
        ])
        .unwrap();

        let mesh = recipe
            .evaluate(&bag(), &TessellationSettings::standard(), &UnavailableCsg)
            .unwrap();
        assert!((mesh.bounds().center().x - 5.0).abs() < 1e-12);
        assert!((mesh.signed_volume() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn forward_and_self_references_are_rejected() {
        let extrude = RecipeOp::Extrude {
            profile: ProfileSpec::Circle {
                radius: Expr::Const(1.0),
            },
            direction: [0.0, 0.0, 1.0],
            depth: Expr::Const(1.0),
        };
        assert_eq!(
            GeometryRecipe::new(vec![
                extrude.clone(),
                RecipeOp::Translate {
                    source: 1, // itself
                    offset: [Expr::Const(0.0), Expr::Const(0.0), Expr::Const(0.0)],
                },
            ]),
            Err(FamilyError::BadRecipeReference {
                index: 1,
                referenced: 1
            })
        );
        assert_eq!(GeometryRecipe::new(vec![]), Err(FamilyError::EmptyRecipe));
        assert_eq!(
            GeometryRecipe::with_output(vec![extrude], 7),
            Err(FamilyError::BadRecipeOutput(7))
        );
    }

    #[test]
    fn a_boolean_step_fails_loudly_without_a_backend() {
        let extrude = |size: f64| RecipeOp::Extrude {
            profile: ProfileSpec::Rectangle {
                width: Expr::Const(size),
                depth: Expr::Const(size),
            },
            direction: [0.0, 0.0, 1.0],
            depth: Expr::Const(size),
        };
        let recipe = GeometryRecipe::new(vec![
            extrude(2.0),
            extrude(1.0),
            RecipeOp::Difference { a: 0, b: 1 },
        ])
        .unwrap();

        assert_eq!(
            recipe.evaluate(&bag(), &TessellationSettings::standard(), &UnavailableCsg),
            Err(FamilyError::Geometry(
                cadforge_geom::GeometryError::CsgUnavailable
            ))
        );
    }

    #[test]
    fn a_single_extrusion_becomes_a_swept_solid_without_tessellating() {
        let recipe = GeometryRecipe::single_extrusion(
            ProfileSpec::Rectangle {
                width: Expr::param("Width"),
                depth: Expr::param("Thickness"),
            },
            [0.0, 0.0, 1.0],
            Expr::param("Height"),
        )
        .unwrap();

        let r = recipe
            .to_representation(&bag(), &TessellationSettings::standard(), &UnavailableCsg)
            .unwrap();
        assert!(r.is_native_parametric());
        assert!(r.is_valid());

        let Representation::ExtrudedAreaSolid {
            profile,
            direction,
            depth,
        } = &r
        else {
            panic!("expected a swept solid");
        };
        assert_eq!(
            profile.len(),
            4,
            "a rectangle stays four points, not triangles"
        );
        assert_eq!(*direction, [0.0, 0.0, 1.0]);
        assert!((depth - 2.1).abs() < 1e-12);
    }

    #[test]
    fn a_multi_step_recipe_degrades_visibly_to_triangles() {
        let recipe = GeometryRecipe::new(vec![
            RecipeOp::Extrude {
                profile: ProfileSpec::Rectangle {
                    width: Expr::Const(1.0),
                    depth: Expr::Const(1.0),
                },
                direction: [0.0, 0.0, 1.0],
                depth: Expr::Const(1.0),
            },
            RecipeOp::Translate {
                source: 0,
                offset: [Expr::Const(2.0), Expr::Const(0.0), Expr::Const(0.0)],
            },
        ])
        .unwrap();

        let r = recipe
            .to_representation(&bag(), &TessellationSettings::standard(), &UnavailableCsg)
            .unwrap();
        assert!(!r.is_native_parametric());
        assert!(r.is_valid());
        assert_eq!(r.ifc_representation_type(), "Tessellation");
    }

    #[test]
    fn a_zero_depth_extrusion_is_refused_rather_than_written() {
        let recipe = GeometryRecipe::single_extrusion(
            ProfileSpec::Rectangle {
                width: Expr::Const(1.0),
                depth: Expr::Const(1.0),
            },
            [0.0, 0.0, 1.0],
            Expr::Const(0.0),
        )
        .unwrap();
        assert_eq!(
            recipe.to_representation(&bag(), &TessellationSettings::standard(), &UnavailableCsg),
            Err(FamilyError::Geometry(
                cadforge_geom::GeometryError::InvalidDepth(0.0)
            ))
        );
    }

    #[test]
    fn evaluation_is_deterministic() {
        let recipe = GeometryRecipe::single_extrusion(
            ProfileSpec::Circle {
                radius: Expr::param("Thickness"),
            },
            [0.0, 0.0, 1.0],
            Expr::param("Height"),
        )
        .unwrap();
        let tess = TessellationSettings::standard();
        let a = recipe.evaluate(&bag(), &tess, &UnavailableCsg).unwrap();
        let b = recipe.evaluate(&bag(), &tess, &UnavailableCsg).unwrap();
        assert_eq!(a, b);
    }
}
