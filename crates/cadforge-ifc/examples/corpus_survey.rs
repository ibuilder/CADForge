//! Survey the IFC conformance corpus.
//!
//! Phase 2b needs to know what real files actually contain before a projection layer is
//! written for them. Guessing produces a reader that handles the entities the author
//! imagined, and CADForge already has a good list of those — it writes them.
//!
//! This runs over `corpus/` (see `tools/fetch_corpus.py`) and answers four questions:
//!
//! 1. Does CADForge's own schema detection work on files it did not write?
//! 2. Does `ifc-lite-core` parse them, and how fast?
//! 3. Which product classes appear, and how many does `IfcClass` model natively?
//! 4. Which representation types appear — how much is swept solids that survive as editable
//!    parametric geometry, and how much is B-rep or tessellation that cannot?
//!
//! That last number is the important one. It is the ceiling on how much of a real model
//! CADForge can round-trip without degrading it.
//!
//!     python tools/fetch_corpus.py
//!     cargo run --release -p cadforge-ifc --example corpus_survey

use cadforge_core::IfcClass;
use cadforge_ifc::IfcSchema;
use ifc_lite_core::{build_entity_index, AttributeValue, EntityDecoder, EntityScanner, IfcType};
use std::collections::BTreeMap;
use std::time::Instant;

fn main() {
    let corpus = std::path::Path::new("corpus");
    if !corpus.is_dir() {
        eprintln!("no corpus/ directory — run: python tools/fetch_corpus.py");
        std::process::exit(1);
    }

    let mut files: Vec<_> = std::fs::read_dir(corpus)
        .expect("corpus is readable")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ifc")))
        .collect();
    files.sort();

    if files.is_empty() {
        eprintln!("corpus/ is empty — run: python tools/fetch_corpus.py");
        std::process::exit(1);
    }

    println!("IFC corpus survey — {} files\n", files.len());
    println!(
        "{:<42} {:>8} {:>10} {:>9} {:>8} {:>7}",
        "file", "schema", "entities", "products", "MB/s", "native"
    );
    println!("{}", "-".repeat(88));

    let mut all_products: BTreeMap<String, usize> = BTreeMap::new();
    let mut all_reprs: BTreeMap<String, usize> = BTreeMap::new();
    let mut unmodelled: BTreeMap<String, usize> = BTreeMap::new();
    let mut schema_failures = Vec::new();
    let mut total_bytes = 0usize;
    let mut total_entities = 0usize;
    let mut total_products = 0usize;
    let mut total_native = 0usize;

    for path in &files {
        let bytes = std::fs::read(path).expect("file is readable");
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        total_bytes += bytes.len();

        // Question 1: does our own header detection cope with a file we did not write?
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
        let schema = match IfcSchema::detect(&head) {
            Ok(s) => s.header_name().to_string(),
            Err(e) => {
                schema_failures.push((name.clone(), e.to_string()));
                "UNKNOWN".to_string()
            }
        };

        // Question 2: does ifc-lite parse it, and how fast?
        let started = Instant::now();
        let index = build_entity_index(&bytes);
        let mut decoder = EntityDecoder::with_index(&bytes, index);

        let mut entities = 0usize;
        let mut products = 0usize;
        let mut native = 0usize;
        let mut shape_ids = Vec::new();

        let mut scanner = EntityScanner::new(&bytes);
        while let Some((id, type_name, _s, _e)) = scanner.next_entity() {
            entities += 1;
            let ifc_type = IfcType::from_str(type_name);

            // Question 3: which products, and do we model them?
            if ifc_type.is_subtype_of(IfcType::IfcProduct) {
                products += 1;
                *all_products.entry(type_name.to_string()).or_default() += 1;
                if !models_natively(type_name) {
                    *unmodelled.entry(type_name.to_string()).or_default() += 1;
                }
            }
            if ifc_type == IfcType::IfcShapeRepresentation {
                shape_ids.push(id);
            }
        }

        // Question 4: how much geometry survives as editable parametric form?
        for id in &shape_ids {
            let Ok(shape) = decoder.decode_by_id(*id) else {
                continue;
            };
            let kind = shape
                .ifc_type
                .attribute_index("RepresentationType")
                .and_then(|i| shape.attributes.get(i))
                .and_then(|a| match a {
                    AttributeValue::String(s) => Some(s.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| "(unset)".into());
            if kind == "SweptSolid" {
                native += 1;
            }
            *all_reprs.entry(kind).or_default() += 1;
        }

        let elapsed = started.elapsed();
        let megabytes = bytes.len() as f64 / 1_048_576.0;
        let native_pct = if shape_ids.is_empty() {
            0.0
        } else {
            100.0 * native as f64 / shape_ids.len() as f64
        };

        println!(
            "{:<42} {:>8} {:>10} {:>9} {:>8.0} {:>6.0}%",
            truncate(&name, 42),
            schema,
            entities,
            products,
            megabytes / elapsed.as_secs_f64(),
            native_pct
        );

        total_entities += entities;
        total_products += products;
        total_native += native;
    }

    println!("{}", "-".repeat(88));
    println!(
        "{:<42} {:>8} {:>10} {:>9}",
        format!("{} files, {:.1} MB", files.len(), total_bytes as f64 / 1e6),
        "",
        total_entities,
        total_products
    );

    // ---- schema detection ---------------------------------------------------------------
    println!("\n1. schema detection");
    if schema_failures.is_empty() {
        println!("   PASS  every file identified from its header");
    } else {
        for (name, reason) in &schema_failures {
            println!("   FAIL  {name}: {reason}");
        }
    }

    // ---- product coverage ---------------------------------------------------------------
    println!("\n2. product classes ({} distinct)", all_products.len());
    let modelled: usize = all_products
        .iter()
        .filter(|(k, _)| models_natively(k))
        .map(|(_, v)| v)
        .sum();
    println!(
        "   {modelled} of {total_products} product instances ({:.0}%) map to a native IfcClass",
        100.0 * modelled as f64 / total_products.max(1) as f64
    );
    println!("\n   most common we do NOT model natively:");
    let mut ranked: Vec<_> = unmodelled.iter().collect();
    ranked.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
    for (name, count) in ranked.iter().take(12) {
        println!("   {count:>7}  {name}");
    }
    if ranked.len() > 12 {
        println!("   {:>7}  … {} more classes", "", ranked.len() - 12);
    }

    // ---- representation types -----------------------------------------------------------
    let total_shapes: usize = all_reprs.values().sum();
    println!("\n3. representation types ({total_shapes} shape representations)");
    let mut ranked: Vec<_> = all_reprs.iter().collect();
    ranked.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
    for (kind, count) in &ranked {
        let share = 100.0 * **count as f64 / total_shapes.max(1) as f64;
        let native = if kind.as_str() == "SweptSolid" {
            "  <- stays parametric"
        } else {
            ""
        };
        println!("   {count:>7}  {share:>5.1}%  {kind}{native}");
    }

    println!(
        "\n   {total_native} of {total_shapes} ({:.0}%) are swept solids — the share that could \
         round-trip\n   through CADForge without degrading to a triangle set.",
        100.0 * total_native as f64 / total_shapes.max(1) as f64
    );
}

/// Whether `IfcClass` has a dedicated variant for this entity, rather than falling back to
/// `IfcClass::Other`.
///
/// `Other` is not a failure — an imported entity keeps its name and round-trips — but it does
/// mean CADForge cannot author or reason about it.
fn models_natively(entity: &str) -> bool {
    let upper = entity.to_ascii_uppercase();
    [
        IfcClass::Project,
        IfcClass::Site,
        IfcClass::Building,
        IfcClass::BuildingStorey,
        IfcClass::Space,
        IfcClass::Wall,
        IfcClass::Slab,
        IfcClass::Roof,
        IfcClass::Column,
        IfcClass::Beam,
        IfcClass::Door,
        IfcClass::Window,
        IfcClass::Stair,
        IfcClass::Covering,
        IfcClass::Furniture,
        IfcClass::OpeningElement,
        IfcClass::BuildingElementProxy,
    ]
    .iter()
    .any(|c| c.ifc_name().to_ascii_uppercase() == upper)
        // IfcWallStandardCase is a wall in every sense that matters here.
        || upper == "IFCWALLSTANDARDCASE"
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n - 1])
    }
}
