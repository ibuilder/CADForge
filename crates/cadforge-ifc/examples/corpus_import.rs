//! Import every file in the corpus, then export and re-import each one.
//!
//! The unit tests round-trip files CADForge wrote, which proves the reader and the writer
//! agree with each other. This proves the reader survives files written by somebody else —
//! and then that what comes back out can be read again, which is the only test that catches
//! information quietly lost in the middle.
//!
//!     python tools/fetch_corpus.py
//!     cargo run --release -p cadforge-ifc --example corpus_import

use cadforge_core::Model;
use cadforge_ifc::backend::ImportWarning;
use cadforge_ifc::{ExportContext, IfcBackend, IfcLiteBackend, IfcSchema, SpfBackend};
use std::collections::BTreeMap;
use std::time::Instant;

fn main() {
    let corpus = std::path::Path::new("corpus");
    if !corpus.is_dir() {
        eprintln!("no corpus/ — run: python tools/fetch_corpus.py");
        std::process::exit(1);
    }

    let mut files: Vec<_> = std::fs::read_dir(corpus)
        .expect("corpus is readable")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ifc")))
        .collect();
    files.sort();

    println!("IFC import over {} corpus files\n", files.len());
    println!(
        "{:<40} {:>7} {:>9} {:>8} {:>8} {:>9} {:>8}",
        "file", "schema", "elements", "geom", "warn", "MB/s", "re-read"
    );
    println!("{}", "-".repeat(96));

    let reader = IfcLiteBackend::new();
    let mut warning_kinds: BTreeMap<&str, usize> = BTreeMap::new();
    let mut unsupported: BTreeMap<String, usize> = BTreeMap::new();
    let mut totals = (0usize, 0usize, 0usize, 0usize); // elements, geom failures, ok, reread ok

    for path in &files {
        let bytes = std::fs::read(path).expect("readable");
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let megabytes = bytes.len() as f64 / 1_048_576.0;

        let mut model = Model::new();
        let started = Instant::now();
        let report = match reader.read(&bytes, &mut model) {
            Ok(report) => report,
            Err(e) => {
                println!("{:<40} {:>7}  IMPORT FAILED: {e}", truncate(&name, 40), "-");
                continue;
            }
        };
        let elapsed = started.elapsed();

        for warning in &report.warnings {
            let kind = match warning {
                ImportWarning::DuplicateGlobalId(_) => "duplicate GlobalId",
                ImportWarning::InvalidGlobalId { .. } => "invalid GlobalId",
                ImportWarning::GeometryFailed { .. } => "geometry failed",
                ImportWarning::DanglingReference { .. } => "dangling reference",
                ImportWarning::UnsupportedEntity { entity, count } => {
                    *unsupported.entry(entity.clone()).or_default() += count;
                    "unsupported entity"
                }
            };
            *warning_kinds.entry(kind).or_default() += 1;
        }

        // Round two: what we imported, written back out, and read again. Anything the model
        // could not hold shows up here as a drop in element count.
        let exported = SpfBackend::new(ExportContext::named(&name).at("2026-08-27T00:00:00"))
            .write(&model, IfcSchema::Ifc4);
        let reread = match exported {
            Ok(bytes) => {
                let mut second = Model::new();
                match reader.read(&bytes, &mut second) {
                    Ok(_) => {
                        // The writer synthesises project/site/building, so growth is expected;
                        // what matters is that nothing authored was lost.
                        let kept = model
                            .iter()
                            .filter(|e| second.contains(&e.global_id))
                            .count();
                        format!("{}/{}", kept, model.len())
                    }
                    Err(_) => "read err".into(),
                }
            }
            Err(_) => "write err".into(),
        };
        let intact = reread
            .split('/')
            .next()
            .and_then(|n| n.parse::<usize>().ok())
            == Some(model.len());

        println!(
            "{:<40} {:>7} {:>9} {:>8} {:>8} {:>9.0} {:>8}",
            truncate(&name, 40),
            report.schema.header_name(),
            report.elements,
            report.geometry_failures,
            report.warnings.len(),
            megabytes / elapsed.as_secs_f64(),
            reread
        );

        totals.0 += report.elements;
        totals.1 += report.geometry_failures;
        totals.2 += 1;
        if intact {
            totals.3 += 1;
        }
    }

    println!("{}", "-".repeat(96));
    println!(
        "{:<40} {:>7} {:>9} {:>8}",
        format!("{}/{} imported", totals.2, files.len()),
        "",
        totals.0,
        totals.1
    );

    println!("\nwarnings by kind");
    if warning_kinds.is_empty() {
        println!("   none");
    }
    for (kind, count) in &warning_kinds {
        println!("   {count:>6}  {kind}");
    }

    println!("\nmost common entities with no native IfcClass");
    let mut ranked: Vec<_> = unsupported.iter().collect();
    ranked.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
    for (entity, count) in ranked.iter().take(10) {
        println!("   {count:>6}  {entity}");
    }

    println!(
        "\n{} of {} files survived import -> export -> import with every element intact.",
        totals.3, totals.2
    );
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n - 1])
    }
}
