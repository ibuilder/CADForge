"""Download the IFC conformance corpus.

The corpus is the gate on Phase 2b. Everything CADForge has round-tripped so far it wrote
itself, which proves the format is self-consistent and says nothing about files produced by
somebody else's exporter.

These are buildingSMART's own certification sample models — IFC4 and IFC4X3_ADD2, spanning
architecture, structural, HVAC, plumbing, road, rail, bridge, and landscaping. They are not
malformed consultant exports (that class of file still has to be sourced separately), but they
are real third-party files with real diversity, and they exercise entities CADForge does not
model natively.

The files are **not committed**. They belong to buildingSMART, they are 16.7 MB, and a corpus
that lives in git history is a corpus nobody can ever prune. This script fetches them into
`corpus/`, which is gitignored, and writes a manifest with sizes and checksums so a run can be
reproduced.

    python tools/fetch_corpus.py            # fetch anything missing
    python tools/fetch_corpus.py --verify   # check what is on disk against the manifest
    python tools/fetch_corpus.py --force    # re-download everything
"""

import argparse
import hashlib
import json
import pathlib
import sys
import urllib.error
import urllib.parse
import urllib.request

REPO = "buildingSMART/Sample-Test-Files"
REF = "main"
ROOT = pathlib.Path(__file__).resolve().parent.parent
CORPUS = ROOT / "corpus"
MANIFEST = CORPUS / "manifest.json"

# buildingSMART publishes these under CC-BY-ND-4.0 per the repository. They are redistributed
# here by download only, never vendored into this repository.
ATTRIBUTION = (
    "buildingSMART International — Sample-Test-Files "
    "(https://github.com/buildingSMART/Sample-Test-Files)"
)


def tree():
    """Every .ifc blob in the source repository, with its size."""
    url = f"https://api.github.com/repos/{REPO}/git/trees/{REF}?recursive=1"
    request = urllib.request.Request(url, headers={"Accept": "application/vnd.github+json"})
    with urllib.request.urlopen(request, timeout=60) as response:
        data = json.load(response)
    if data.get("truncated"):
        print("warning: the GitHub tree response was truncated", file=sys.stderr)
    return [
        {"path": entry["path"], "size": entry.get("size", 0)}
        for entry in data.get("tree", [])
        if entry["type"] == "blob" and entry["path"].lower().endswith(".ifc")
    ]


def local_name(path):
    """Flatten `IFC 4.0.2.1 (IFC 4)/PCERT-Sample-Scene/Infra-Road.ifc` into something a shell
    can handle without quoting: `ifc4__infra-road.ifc`."""
    parts = path.split("/")
    schema = "ifc4x3" if "4X3" in parts[0].upper() or "4.3" in parts[0] else "ifc4"
    stem = parts[-1].removesuffix(".ifc").removesuffix(".IFC")
    return f"{schema}__{stem.lower().replace(' ', '-')}.ifc"


def download(path):
    url = (
        f"https://raw.githubusercontent.com/{REPO}/{REF}/"
        + urllib.parse.quote(path)
    )
    with urllib.request.urlopen(url, timeout=180) as response:
        return response.read()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify", action="store_true", help="check disk against the manifest")
    parser.add_argument("--force", action="store_true", help="re-download everything")
    args = parser.parse_args()

    if args.verify:
        return verify()

    CORPUS.mkdir(exist_ok=True)
    entries = tree()
    print(f"{len(entries)} IFC files in {REPO}\n")

    manifest = {"source": REPO, "ref": REF, "attribution": ATTRIBUTION, "files": {}}
    fetched = skipped = 0

    for entry in sorted(entries, key=lambda e: e["path"]):
        name = local_name(entry["path"])
        target = CORPUS / name

        if target.exists() and not args.force:
            data = target.read_bytes()
            skipped += 1
            status = "have"
        else:
            try:
                data = download(entry["path"])
            except urllib.error.URLError as e:
                print(f"  FAIL  {name}: {e}", file=sys.stderr)
                continue
            target.write_bytes(data)
            fetched += 1
            status = "get "

        manifest["files"][name] = {
            "source_path": entry["path"],
            "bytes": len(data),
            "sha256": digest(data),
        }
        print(f"  {status}  {len(data)/1024:8.0f} KB  {name}")

    MANIFEST.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    total = sum(f["bytes"] for f in manifest["files"].values())
    print(f"\n{fetched} downloaded, {skipped} already present — {total/1e6:.1f} MB in {CORPUS}")
    print(f"manifest: {MANIFEST.relative_to(ROOT)}")
    print(f"\nThese files are {ATTRIBUTION}.")
    print("They are downloaded, never committed. See the module docstring.")
    return 0


def verify():
    if not MANIFEST.exists():
        print("no manifest — run without --verify first", file=sys.stderr)
        return 1
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    bad = missing = 0
    for name, meta in sorted(manifest["files"].items()):
        target = CORPUS / name
        if not target.exists():
            print(f"  MISSING  {name}")
            missing += 1
            continue
        actual = digest(target.read_bytes())
        if actual != meta["sha256"]:
            print(f"  CHANGED  {name}")
            bad += 1
    total = len(manifest["files"])
    print(f"\n{total - bad - missing}/{total} verified, {bad} changed, {missing} missing")
    return 1 if (bad or missing) else 0


if __name__ == "__main__":
    sys.exit(main())
