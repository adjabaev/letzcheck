# This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
# If a copy of the MPL was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Downloads the three LOD open datasets (CC0) from data.public.lu into one folder.

Usage: python3 tools/fetch_lod.py [--version YYMMDD | --latest] [--out data/lod]

The default is the pinned release the tests were measured on; its zips are checked against the
SHA-256 sums below. --latest takes the newest release published for all three datasets (no checksum).
Writes <out>/art/new_lod-art.xml, <out>/tab/new_lod-tab.xml, <out>/search/new_lod-search.xml and
<out>/VERSION. Standard library only.
"""
import argparse, hashlib, io, json, os, sys, urllib.request, zipfile

API = "https://data.public.lu/api/1/datasets/"
DATASETS = {  # folder -> data.public.lu dataset slug
    "art": "letzebuerger-online-dictionnaire-lod-linguistesch-daten",
    "tab": "letzebuerger-online-dictionnaire-lod-flexiounstabellen",
    "search": "letzebuerger-online-dictionnaire-lod-index-vun-der-sich-funktioun",
}
PINNED = "260727"
SHA256 = {
    "260727-new-lod-art.zip": "61d01144f5477eebbd0440b21f77656aa21119c7d72ba193edaeeebe90be901c",
    "260727-new-lod-tab.zip": "d66fd175b5558c761a3dee934505c9c7cee31ccdf9e7679e212bae902451d1e8",
    "260727-new-lod-search.zip": "5da8d3c86be5ef9556eeb8dce9e92aef879ff1bc121dc8785c1dc83d2996d5b9",
}
UA = {"User-Agent": "letzcheck-fetch/1"}

def get(url):
    with urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=120) as r:
        return r.read()

def releases(slug):
    """{YYMMDD: (title, url)} for the zip resources of a dataset."""
    d = json.loads(get(API + slug + "/"))
    if d.get("license") != "cc-zero":
        sys.exit(f"{slug}: licence is {d.get('license')!r}, expected cc-zero; stopping")
    out = {}
    for r in d["resources"]:
        t = r["title"]
        if t.endswith(".zip") and t[:6].isdigit(): out.setdefault(t[:6], (t, r["url"]))
    return out

def main():
    ap = argparse.ArgumentParser()
    g = ap.add_mutually_exclusive_group()
    g.add_argument("--version", default=PINNED, help="release date YYMMDD (default: pinned %s)" % PINNED)
    g.add_argument("--latest", action="store_true", help="newest release common to the three datasets")
    ap.add_argument("--out", default="data/lod")
    a = ap.parse_args()

    rel = {k: releases(slug) for k, slug in DATASETS.items()}
    common = set.intersection(*(set(r) for r in rel.values()))
    ver = max(common) if a.latest else a.version
    if ver not in common:
        sys.exit(f"release {ver} is not available for all three datasets; available: {', '.join(sorted(common))}")

    for k, r in rel.items():
        title, url = r[ver]
        print(f"{k:6} {title}", file=sys.stderr)
        blob = get(url)
        want = SHA256.get(title)
        if want and hashlib.sha256(blob).hexdigest() != want:
            sys.exit(f"{title}: checksum mismatch")
        dest = os.path.join(a.out, k)
        os.makedirs(dest, exist_ok=True)
        with zipfile.ZipFile(io.BytesIO(blob)) as z:
            names = [n for n in z.namelist() if n.endswith(".xml") and not n.startswith("__MACOSX")]
            if len(names) != 1: sys.exit(f"{title}: expected one XML file, found {names}")
            with z.open(names[0]) as src, open(os.path.join(dest, f"new_lod-{k}.xml"), "wb") as dst:
                while chunk := src.read(1 << 20): dst.write(chunk)
    open(os.path.join(a.out, "VERSION"), "w").write(ver + "\n")
    print(f"LOD {ver} -> {a.out}", file=sys.stderr)

if __name__ == "__main__":
    main()
