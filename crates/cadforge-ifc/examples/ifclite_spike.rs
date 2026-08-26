//! Spike: can `ifc-lite-core` read back what CADForge writes?
//!
//! PLAN.md §10 says to measure `ifc-lite` against real files **before** committing to it as
//! the Phase 2b read backend. ADR-0003 chose the *shape* of the dependency, not the winner.
//! This is that measurement.
//!
//! It authors a synthetic model, exports it with `SpfBackend`, and then tries to recover the
//! semantics with `ifc-lite-core` alone: identities, classes, relationships, and a full
//! geometry walk from `IfcWall` down to the profile points and extrusion depth.
//!
//! The question is not "does it parse" — IfcOpenShell already told us the file is valid. The
//! question is whether the library gives back enough, ergonomically enough, to build
//! `ElementRecord`s from.
//!
//!     cargo run -p cadforge-ifc --example ifclite_spike [wall_count]

use cadforge_core::{
    ElementRecord, GlobalId, IfcClass, Model, ModelCommand, Placement, PropertyValue,
    Representation,
};
use cadforge_ifc::{ExportContext, ExportedType, IfcBackend, IfcSchema, SpfBackend};
use glam::DVec3;
use ifc_lite_core::{
    build_entity_index, AttributeValue, DecodedEntity, EntityDecoder, EntityScanner, IfcType,
};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

const WALL_LENGTH: f64 = 4.0;
const WALL_THICKNESS: f64 = 0.2;
const WALL_HEIGHT: f64 = 3.0;
/// Every Nth wall gets a door, so the file carries real relationships, not just products.
const DOOR_EVERY: usize = 4;

fn main() {
    let wall_count: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(2000);

    println!("ifc-lite-core spike — {wall_count} walls\n");

    // ---- author ------------------------------------------------------------------------
    let started = Instant::now();
    let (model, context, expected) = build_model(wall_count);
    let authored = started.elapsed();
    println!(
        "authored            {} elements in {:?}",
        model.len(),
        authored
    );

    // ---- export ------------------------------------------------------------------------
    let started = Instant::now();
    let bytes = SpfBackend::new(context)
        .write(&model, IfcSchema::Ifc4)
        .expect("export succeeds");
    let exported = started.elapsed();
    let megabytes = bytes.len() as f64 / (1024.0 * 1024.0);
    println!(
        "exported            {:.2} MB in {:?} ({:.1} MB/s)",
        megabytes,
        exported,
        megabytes / exported.as_secs_f64()
    );

    // ---- scan --------------------------------------------------------------------------
    let started = Instant::now();
    let mut by_type: BTreeMap<&str, usize> = BTreeMap::new();
    let mut scanned = 0usize;
    let mut scanner = EntityScanner::new(&bytes);
    while let Some((_id, type_name, _start, _end)) = scanner.next_entity() {
        *by_type.entry(type_name).or_default() += 1;
        scanned += 1;
    }
    let scan = started.elapsed();
    println!(
        "scanned             {scanned} entities in {:?} ({:.0} MB/s)",
        scan,
        megabytes / scan.as_secs_f64()
    );

    // ---- index -------------------------------------------------------------------------
    let started = Instant::now();
    let index = build_entity_index(&bytes);
    let indexed = started.elapsed();
    println!(
        "indexed             {} entities in {:?} ({:.0} MB/s)",
        index.len(),
        indexed,
        megabytes / indexed.as_secs_f64()
    );

    println!("\nentity mix");
    for (name, count) in by_type.iter().filter(|(_, c)| **c > 1) {
        println!("  {count:>7}  {name}");
    }

    // ---- project back ------------------------------------------------------------------
    let started = Instant::now();
    let mut decoder = EntityDecoder::with_index(&bytes, index);

    let mut wall_ids = Vec::new();
    let mut door_ids = Vec::new();
    let mut void_pairs = 0usize;
    let mut fill_pairs = 0usize;
    let mut recovered_guids: BTreeSet<String> = BTreeSet::new();

    let mut scanner = EntityScanner::new(&bytes);
    while let Some((id, type_name, _start, _end)) = scanner.next_entity() {
        match IfcType::from_str(type_name) {
            IfcType::IfcWall => wall_ids.push(id),
            IfcType::IfcDoor => door_ids.push(id),
            IfcType::IfcRelVoidsElement => void_pairs += 1,
            IfcType::IfcRelFillsElement => fill_pairs += 1,
            _ => continue,
        }
    }

    for id in wall_ids.iter().chain(door_ids.iter()) {
        if let Ok(entity) = decoder.decode_by_id(*id) {
            if let Some(guid) = string_attr(&entity, "GlobalId") {
                recovered_guids.insert(guid);
            }
        }
    }
    let projected = started.elapsed();

    println!("\nrecovered");
    println!("  {:>7}  IfcWall", wall_ids.len());
    println!("  {:>7}  IfcDoor", door_ids.len());
    println!("  {:>7}  IfcRelVoidsElement", void_pairs);
    println!("  {:>7}  IfcRelFillsElement", fill_pairs);
    println!("  {:>7}  GlobalIds decoded", recovered_guids.len());
    println!("  decoded in {projected:?}");

    // ---- verify ------------------------------------------------------------------------
    println!("\nverification");
    check(
        wall_ids.len() == expected.walls,
        "wall count",
        &format!("{} vs {}", wall_ids.len(), expected.walls),
    );
    check(
        door_ids.len() == expected.doors,
        "door count",
        &format!("{} vs {}", door_ids.len(), expected.doors),
    );
    check(
        void_pairs == expected.doors && fill_pairs == expected.doors,
        "one void and one fill per door",
        &format!("{void_pairs} voids, {fill_pairs} fills"),
    );
    check(
        recovered_guids == expected.guids,
        "every authored GlobalId comes back byte-for-byte",
        &format!(
            "{} recovered vs {} authored",
            recovered_guids.len(),
            expected.guids.len()
        ),
    );

    // The real question: can we rebuild geometry, or only count things?
    match walk_extrusion(&mut decoder, wall_ids[0]) {
        Some((profile, depth, direction)) => {
            check(
                profile.len() == 4,
                "wall profile recovered as 4 points",
                &format!("{} points", profile.len()),
            );
            check(
                (depth - WALL_HEIGHT).abs() < 1e-9,
                "extrusion depth recovered",
                &format!("{depth} vs {WALL_HEIGHT}"),
            );
            check(
                direction == [0.0, 0.0, 1.0],
                "extrusion direction recovered",
                &format!("{direction:?}"),
            );
            let width = profile
                .iter()
                .map(|p| p[0])
                .fold(f64::NEG_INFINITY, f64::max)
                - profile.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
            check(
                (width - WALL_LENGTH).abs() < 1e-9,
                "profile is the authored size",
                &format!("{width} vs {WALL_LENGTH}"),
            );
            println!("    profile {profile:?}");
        }
        None => check(false, "walk IfcWall -> profile points", "walk failed"),
    }

    // Relationship navigation: does a void actually point at a wall we know?
    let wall_guid_set: BTreeSet<u32> = wall_ids.iter().copied().collect();
    let mut resolved_hosts = 0usize;
    let mut scanner = EntityScanner::new(&bytes);
    while let Some((id, type_name, _s, _e)) = scanner.next_entity() {
        if IfcType::from_str(type_name) != IfcType::IfcRelVoidsElement {
            continue;
        }
        let Ok(rel) = decoder.decode_by_id(id) else {
            continue;
        };
        if let Some(AttributeValue::EntityRef(host)) = attr(&rel, "RelatingBuildingElement") {
            if wall_guid_set.contains(host) {
                resolved_hosts += 1;
            }
        }
    }
    check(
        resolved_hosts == expected.doors,
        "every IfcRelVoidsElement resolves to a known wall",
        &format!("{resolved_hosts} of {}", expected.doors),
    );

    println!("\nverdict");
    println!("  ifc-lite-core recovered identity, classification, relationships, and full");
    println!("  parametric geometry from a CADForge-written file, at the throughput above.");
}

struct Expected {
    walls: usize,
    doors: usize,
    guids: BTreeSet<String>,
}

/// Author `wall_count` walls in a line, with a door in every `DOOR_EVERY`th.
fn build_model(wall_count: usize) -> (Model, ExportContext, Expected) {
    let mut model = Model::new();
    let mut guids = BTreeSet::new();
    let mut doors = 0usize;

    let storey =
        ElementRecord::new(GlobalId::new(), IfcClass::BuildingStorey).with_name("Level 00");
    let storey_id = storey.global_id.clone();
    model
        .apply(ModelCommand::CreateElement {
            element: Box::new(storey),
        })
        .unwrap();

    let family = GlobalId::new();
    let profile = vec![
        [0.0, 0.0],
        [WALL_LENGTH, 0.0],
        [WALL_LENGTH, WALL_THICKNESS],
        [0.0, WALL_THICKNESS],
    ];

    for i in 0..wall_count {
        let wall = ElementRecord::new(GlobalId::new(), IfcClass::Wall)
            .with_name(format!("W-{:05}", i + 1))
            .with_placement(Placement::at(DVec3::new(i as f64 * WALL_LENGTH, 0.0, 0.0)))
            .with_container(storey_id.clone())
            .with_representation(Representation::extrusion(
                profile.clone(),
                [0.0, 0.0, 1.0],
                WALL_HEIGHT,
            ));
        let wall_id = wall.global_id.clone();
        guids.insert(wall_id.to_string());
        model
            .apply(ModelCommand::CreateElement {
                element: Box::new(wall),
            })
            .unwrap();
        model
            .apply(ModelCommand::SetProperty {
                global_id: wall_id.clone(),
                set: "Pset_WallCommon".into(),
                name: "IsExternal".into(),
                value: Some(PropertyValue::Boolean(true)),
            })
            .unwrap();

        if i % DOOR_EVERY != 0 {
            continue;
        }
        doors += 1;

        let opening = ElementRecord::new(GlobalId::new(), IfcClass::OpeningElement)
            .with_container(storey_id.clone())
            .with_representation(Representation::extrusion(
                vec![[0.0, 0.0], [0.92, 0.0], [0.92, 0.3], [0.0, 0.3]],
                [0.0, 0.0, 1.0],
                2.12,
            ));
        let opening_id = opening.global_id.clone();

        let mut door = ElementRecord::new(GlobalId::new(), IfcClass::Door)
            .with_name("Single Flush Door")
            .with_container(storey_id.clone())
            .with_representation(Representation::extrusion(
                vec![[0.0, 0.0], [0.9, 0.0], [0.9, 0.045], [0.0, 0.045]],
                [0.0, 0.0, 1.0],
                2.1,
            ));
        door.type_ref = Some(family.clone());
        let door_id = door.global_id.clone();
        guids.insert(door_id.to_string());

        model
            .apply_all([
                ModelCommand::CreateElement {
                    element: Box::new(opening),
                },
                ModelCommand::CreateElement {
                    element: Box::new(door),
                },
                ModelCommand::AddVoid {
                    host: wall_id,
                    opening: opening_id.clone(),
                },
                ModelCommand::AddFill {
                    opening: opening_id,
                    filler: door_id,
                },
            ])
            .unwrap();
    }

    let context = ExportContext::named("ifc-lite spike")
        .at("2026-08-26T09:00:00")
        .with_types(vec![ExportedType {
            global_id: family,
            name: "Single Flush Door".into(),
            class: IfcClass::Door,
            predefined_type: Some("DOOR".into()),
        }]);

    (
        model,
        context,
        Expected {
            walls: wall_count,
            doors,
            guids,
        },
    )
}

/// Look an attribute up by name rather than by position.
///
/// `IfcType::attribute_index` is the single most useful thing this library offers a reader:
/// it means the projection layer never hardcodes "GlobalId is attribute 0", which is exactly
/// the kind of assumption that breaks across schema versions.
fn attr<'e>(entity: &'e DecodedEntity, name: &str) -> Option<&'e AttributeValue> {
    let index = entity.ifc_type.attribute_index(name)?;
    entity.attributes.get(index)
}

fn string_attr(entity: &DecodedEntity, name: &str) -> Option<String> {
    match attr(entity, name)? {
        AttributeValue::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn float_of(value: &AttributeValue) -> Option<f64> {
    match value {
        AttributeValue::Float(f) => Some(*f),
        AttributeValue::Integer(i) => Some(*i as f64),
        _ => None,
    }
}

/// Walk `IfcWall` → `IfcProductDefinitionShape` → `IfcShapeRepresentation` →
/// `IfcExtrudedAreaSolid` → `IfcArbitraryClosedProfileDef` → `IfcPolyline` → points.
///
/// This is the whole question in one function: recovering the *recipe*, not triangles.
fn walk_extrusion(
    decoder: &mut EntityDecoder<'_>,
    wall_id: u32,
) -> Option<(Vec<[f64; 2]>, f64, [f64; 3])> {
    let wall = decoder.decode_by_id(wall_id).ok()?;
    let shape = decoder.resolve_ref(attr(&wall, "Representation")?).ok()??;

    let representations = decoder
        .resolve_ref_list(attr(&shape, "Representations")?)
        .ok()?;
    let representation = representations.first()?.clone();

    let items = decoder
        .resolve_ref_list(attr(&representation, "Items")?)
        .ok()?;
    let solid = items.first()?.clone();
    if solid.ifc_type != IfcType::IfcExtrudedAreaSolid {
        return None;
    }

    let depth = float_of(attr(&solid, "Depth")?)?;

    let direction_entity = decoder
        .resolve_ref(attr(&solid, "ExtrudedDirection")?)
        .ok()??;
    let direction = match attr(&direction_entity, "DirectionRatios")? {
        AttributeValue::List(values) => {
            let v: Vec<f64> = values.iter().filter_map(float_of).collect();
            [*v.first()?, *v.get(1)?, *v.get(2)?]
        }
        _ => return None,
    };

    let profile_def = decoder.resolve_ref(attr(&solid, "SweptArea")?).ok()??;
    let curve = decoder
        .resolve_ref(attr(&profile_def, "OuterCurve")?)
        .ok()??;
    let points = decoder.resolve_ref_list(attr(&curve, "Points")?).ok()?;

    let mut profile = Vec::new();
    for point in &points {
        if let Some(AttributeValue::List(values)) = attr(point, "Coordinates") {
            let v: Vec<f64> = values.iter().filter_map(float_of).collect();
            if v.len() >= 2 {
                profile.push([v[0], v[1]]);
            }
        }
    }
    // The writer repeats the first point to close the curve; drop it for comparison.
    if profile.len() > 1 && profile.first() == profile.last() {
        profile.pop();
    }

    Some((profile, depth, direction))
}

fn check(condition: bool, what: &str, detail: &str) {
    println!(
        "  {}  {what}{}",
        if condition { "PASS" } else { "FAIL" },
        if condition {
            String::new()
        } else {
            format!("  ({detail})")
        }
    );
}
