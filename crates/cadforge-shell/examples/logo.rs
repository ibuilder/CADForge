//! Render the CADForge mark using CADForge.
//!
//! The logo is an anvil, and an anvil in profile is a closed thirteen-point loop with no
//! curves — precisely an `IfcArbitraryClosedProfileDef`. Sweep it and you have an
//! `IfcExtrudedAreaSolid`, the one representation this project writes as native parametric
//! IFC.
//!
//! So the mark is not drawn. It is authored as a profile, swept by the geometry pipeline,
//! and photographed by the renderer, which makes it a slightly obnoxious end-to-end test:
//! the profile is concave enough to exercise ear clipping rather than a triangle fan, and if
//! the winding is ever wrong, back-face culling turns the logo inside out.
//!
//!     cargo run --release -p cadforge-shell --features gpu --example logo

use anyhow::Result;
use cadforge_core::BoundingBox;
use cadforge_geom::{extrude, Profile};
use cadforge_render::{Camera, MeshData, Renderer};
use glam::{DMat4, DVec2, DVec3};

/// The anvil, in the same coordinates as `site/assets/logo.svg` so the two marks stay the
/// same shape: a 128-unit box, y pointing down.
const OUTLINE: [[f64; 2]; 13] = [
    [6.0, 44.0],   // horn tip
    [20.0, 32.0],  // top face, left
    [114.0, 32.0], // top face, right
    [114.0, 52.0], // heel
    [78.0, 52.0],  // step down to the waist
    [78.0, 84.0],
    [98.0, 84.0], // base flare
    [98.0, 104.0],
    [30.0, 104.0],
    [30.0, 84.0],
    [50.0, 84.0],
    [50.0, 52.0],
    [20.0, 52.0],
];

const DEPTH: f64 = 0.34;

fn main() -> Result<()> {
    println!("CADForge mark — authored, swept, rendered\n");

    // SVG space is y-down and 128 wide; the model is y-up and roughly a metre across.
    let profile = Profile::new(
        OUTLINE
            .iter()
            .map(|[x, y]| DVec2::new((x - 64.0) / 100.0, (64.0 - y) / 100.0)),
    )?;

    println!(
        "profile             {} points, {:.4} m² enclosed, {:.3} m perimeter",
        profile.outer().len(),
        profile.area(),
        profile.perimeter()
    );
    println!(
        "triangulation       {} triangles by ear clipping",
        profile.triangulate()?.len()
    );

    let solid = extrude(&profile, DEPTH)?;
    println!(
        "swept solid         {} triangles, {:.5} m³",
        solid.triangle_count(),
        solid.signed_volume()
    );
    // A positive signed volume is the same statement as "the winding is right", which is what
    // keeps back-face culling from hollowing the mark out.
    anyhow::ensure!(
        solid.signed_volume() > 0.0,
        "the profile wound the wrong way"
    );

    // The profile is authored in XY and swept along Z, which leaves the mark lying flat like
    // a floor plan — the camera is Z-up because IFC is. Standing it up in XZ, with the sweep
    // running along Y, is what turns a plan into an elevation.
    let upright = DMat4::from_rotation_x(std::f64::consts::FRAC_PI_2)
        * DMat4::from_translation(DVec3::new(0.0, 0.0, -DEPTH / 2.0));
    let mark = solid.transformed(upright);

    // The working face, hot. A second sweep rather than a texture: slightly deeper than the
    // body so its front face sits proud and cannot z-fight with the one behind it.
    let hot_band = Profile::new(
        // Inset and lifted a hair: coplanar faces z-fight, and the speckle shows up exactly
        // where the band met the body top edge-on.
        [[21.0, 31.4], [113.0, 31.4], [113.0, 39.5], [21.0, 39.5]]
            .iter()
            .map(|[x, y]: &[f64; 2]| DVec2::new((x - 64.0) / 100.0, (64.0 - y) / 100.0)),
    )?;
    let hot_depth = DEPTH + 0.006;
    let hot = extrude(&hot_band, hot_depth)?.transformed(
        DMat4::from_rotation_x(std::f64::consts::FRAC_PI_2)
            * DMat4::from_translation(DVec3::new(0.0, 0.0, -hot_depth / 2.0)),
    );

    let (width, height) = (1024u32, 1024u32);
    let renderer = Renderer::new_headless(width, height)?;
    let mut camera = Camera::default();
    camera.set_viewport(width, height);
    camera.frame(&padded(mark.bounds(), 0.03));
    // Look at the face from slightly off-axis and slightly above: enough to read the sweep
    // depth, not so much that the silhouette stops being an anvil.
    camera.yaw = -std::f64::consts::FRAC_PI_2 + 0.34;
    camera.pitch = 0.20;

    let path = std::path::Path::new("site/assets/logo.png");
    renderer.render_to_png(
        &[
            MeshData {
                positions: &mark.positions,
                normals: &mark.normals,
                indices: &mark.indices,
                color: [0.60, 0.65, 0.73],
            },
            MeshData {
                positions: &hot.positions,
                normals: &hot.normals,
                indices: &hot.indices,
                color: [0.89, 0.52, 0.16],
            },
        ],
        &camera,
        path,
    )?;

    println!("\ngpu                 {}", renderer.adapter_description());
    println!("wrote               {} at {width}×{height}", path.display());
    Ok(())
}

/// Grow a box by a fraction of its largest dimension, so framing leaves a margin.
fn padded(bounds: BoundingBox, fraction: f64) -> BoundingBox {
    let margin = bounds.size().max_element() * fraction;
    BoundingBox::new(
        bounds.min - DVec3::splat(margin),
        bounds.max + DVec3::splat(margin),
    )
}
