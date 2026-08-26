"""Validate a CADForge-exported IFC file against IfcOpenShell.

The unit tests in `crates/cadforge-ifc/src/spf.rs` prove the exported file is *internally
consistent*. They cannot prove anyone else accepts it. This does: it runs the reference
implementation over the output and checks that it parses, satisfies the EXPRESS rules,
resolves every relationship, and generates real geometry.

    pip install ifcopenshell
    cargo run -p cadforge-shell
    python tools/validate_ifc.py out/demo.ifc

Exit code is non-zero on any failure, so it drops straight into CI.
"""

import sys
import ifcopenshell
import ifcopenshell.geom
import ifcopenshell.util.element
import ifcopenshell.util.shape

PATH = sys.argv[1] if len(sys.argv) > 1 else "out/demo.ifc"

failures = []
warnings = []


def check(condition, message, detail=""):
    if condition:
        print(f"  PASS  {message}")
    else:
        failures.append(f"{message} - {detail}")
        print(f"  FAIL  {message}  {detail}")


print(f"IfcOpenShell {ifcopenshell.version}\nfile: {PATH}\n")

# ---- 1. does it open at all -------------------------------------------------------------
print("parse")
try:
    f = ifcopenshell.open(PATH)
except Exception as e:  # noqa: BLE001
    print(f"  FAIL  file could not be opened: {e}")
    sys.exit(1)
check(True, "file opens")
check(f.schema == "IFC4", "schema is IFC4", f"got {f.schema}")
print(f"        {len(list(f))} entity instances")

# ---- 2. schema-level validation ---------------------------------------------------------
print("\nvalidate")
try:
    import ifcopenshell.validate
    logger = ifcopenshell.validate.json_logger()
    ifcopenshell.validate.validate(f, logger, express_rules=True)
    issues = logger.statements
    by_kind = {}
    for issue in issues:
        by_kind.setdefault(str(issue.get("message", ""))[:90], 0)
        by_kind[str(issue.get("message", ""))[:90]] += 1
    check(not issues, "no schema or EXPRESS rule violations", f"{len(issues)} issue(s)")
    for message, count in sorted(by_kind.items(), key=lambda kv: -kv[1])[:12]:
        print(f"          x{count}  {message}")
except ImportError:
    warnings.append("ifcopenshell.validate unavailable")
    print("  SKIP  ifcopenshell.validate not available")

# ---- 3. structure -----------------------------------------------------------------------
print("\nstructure")
project = f.by_type("IfcProject")
check(len(project) == 1, "exactly one IfcProject", f"got {len(project)}")
if project:
    units = project[0].UnitsInContext
    check(units is not None, "project carries a unit assignment")
    length = [u for u in (units.Units if units else []) if getattr(u, "UnitType", "") == "LENGTHUNIT"]
    check(
        bool(length) and length[0].Name == "METRE",
        "length unit is METRE",
        f"got {length[0].Name if length else 'none'}",
    )
    check(bool(project[0].RepresentationContexts), "project carries a representation context")

for kind, expected in [("IfcSite", 1), ("IfcBuilding", 1), ("IfcBuildingStorey", 1)]:
    check(len(f.by_type(kind)) == expected, f"exactly {expected} {kind}", f"got {len(f.by_type(kind))}")

storey = f.by_type("IfcBuildingStorey")[0]
contained = ifcopenshell.util.element.get_decomposition(storey)
print(f"        storey '{storey.Name}' decomposes into {len(contained)} element(s)")

walls = f.by_type("IfcWall")
doors = f.by_type("IfcDoor")
openings = f.by_type("IfcOpeningElement")
check(len(walls) == 4, "4 walls", f"got {len(walls)}")
check(len(doors) == 1, "1 door", f"got {len(doors)}")
check(len(openings) == 1, "1 opening", f"got {len(openings)}")

# ---- 4. relationships -------------------------------------------------------------------
print("\nrelationships")
door = doors[0]
opening = openings[0]

check(bool(opening.VoidsElements), "opening voids a host element")
if opening.VoidsElements:
    host = opening.VoidsElements[0].RelatingBuildingElement
    check(host.is_a("IfcWall"), "the host is a wall", f"got {host.is_a()}")
    print(f"        {host.is_a()} '{host.Name}' <- voided by {opening.is_a()}")

check(bool(opening.HasFillings), "opening is filled")
if opening.HasFillings:
    filler = opening.HasFillings[0].RelatedBuildingElement
    check(filler.id() == door.id(), "the filler is our door", f"got {filler.is_a()}")

check(
    bool(ifcopenshell.util.element.get_container(door)),
    "door resolves to a spatial container",
)
# Test the raw attribute, not get_container(). IfcOpenShell deliberately resolves an
# opening's container *through its host*, so get_container() returning a storey is correct
# behaviour and says nothing about what we wrote. What matters is that we did not list the
# opening in IfcRelContainedInSpatialStructure ourselves, which would be a validation error.
check(
    not opening.ContainedInStructure,
    "opening is not listed in IfcRelContainedInSpatialStructure",
    f"got {opening.ContainedInStructure}",
)
check(
    all(
        not e.is_a("IfcOpeningElement")
        for rel in f.by_type("IfcRelContainedInSpatialStructure")
        for e in rel.RelatedElements
    ),
    "no opening appears in any containment relationship",
)

door_type = ifcopenshell.util.element.get_type(door)
check(door_type is not None and door_type.is_a("IfcDoorType"), "door resolves to an IfcDoorType")
if door_type is not None:
    print(f"        type '{door_type.Name}' predefined {door_type.PredefinedType}")

psets = ifcopenshell.util.element.get_psets(walls[0])
check("Pset_WallCommon" in psets, "wall carries Pset_WallCommon", f"got {list(psets)}")
check(
    psets.get("Pset_WallCommon", {}).get("IsExternal") is True,
    "IsExternal survived as a boolean",
    f"got {psets.get('Pset_WallCommon', {}).get('IsExternal')!r}",
)

# ---- 5. geometry ------------------------------------------------------------------------
print("\ngeometry")
swept = f.by_type("IfcExtrudedAreaSolid")
check(len(swept) == 6, "6 IfcExtrudedAreaSolid", f"got {len(swept)}")
check(
    not f.by_type("IfcTriangulatedFaceSet"),
    "no tessellated fallbacks - everything stayed parametric",
)

settings = ifcopenshell.geom.settings()
generated, failed, volumes = 0, [], {}
for element in walls + doors + openings:
    try:
        shape = ifcopenshell.geom.create_shape(settings, element)
        volume = ifcopenshell.util.shape.get_volume(shape.geometry)
        volumes[f"{element.is_a()} {element.Name or element.GlobalId[:8]}"] = volume
        generated += 1
    except Exception as e:  # noqa: BLE001
        failed.append(f"{element.is_a()} {element.GlobalId}: {e}")

check(
    generated == len(walls) + len(doors) + len(openings),
    f"IfcOpenShell generated geometry for all {len(walls) + len(doors) + len(openings)} elements",
    "; ".join(failed),
)
for name, volume in volumes.items():
    print(f"        {name:<28} {volume:8.4f} m3")

# The door opening really cuts its host, so IfcOpenShell reports LESS wall than CADForge's
# own uncut meshes do. That gap is the point of the check, not a discrepancy to explain away:
#   4 walls uncut          = 15.6000 m3
#   door void through W-01 =  0.3901 m3  (0.92 wide x 0.20 wall x 2.12 high)
#   what a consumer sees   = 15.2099 m3
# If this ever comes back as 15.60, IfcRelVoidsElement stopped working.
UNCUT, VOID = 15.60, 0.92 * 0.20 * 2.12
wall_volume = sum(v for k, v in volumes.items() if k.startswith("IfcWall"))
check(
    abs(wall_volume - (UNCUT - VOID)) < 0.01,
    f"walls come back CUT by the opening ({UNCUT - VOID:.4f} m3, not the uncut {UNCUT})",
    f"got {wall_volume:.4f}",
)
door_volume = sum(v for k, v in volumes.items() if k.startswith("IfcDoor"))
check(
    abs(door_volume - 0.0851) < 0.001,
    "door leaf volume matches (0.0851 m3)",
    f"got {door_volume:.4f}",
)

# ---- 6. round trip ----------------------------------------------------------------------
print("\nround trip")
out = PATH.replace(".ifc", ".roundtrip.ifc")
f.write(out)
g = ifcopenshell.open(out)
check(len(list(g)) == len(list(f)), "re-reading IfcOpenShell's own output preserves entity count")
original_guids = {e.GlobalId for e in f.by_type("IfcRoot")}
roundtrip_guids = {e.GlobalId for e in g.by_type("IfcRoot")}
check(original_guids == roundtrip_guids, "every GlobalId survives the round trip")

print("\n" + "=" * 72)
if failures:
    print(f"{len(failures)} FAILURE(S):")
    for failure in failures:
        print(f"  - {failure}")
    sys.exit(1)
print(f"All checks passed{' (' + ', '.join(warnings) + ')' if warnings else ''}.")
