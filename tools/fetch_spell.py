# This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
# If a copy of the MPL was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Downloads the spellchecker.lu Hunspell dictionary for Luxembourgish (Zenter fir d'Lëtzebuerger
Sprooch, CC0) from its Debian package repository.

Usage: python3 tools/fetch_spell.py [--latest] [--out data/spell]

The default is the pinned package the tests were measured on; it is checked against the SHA-256
below. --latest takes whatever the repository serves now, checked against the repository's own
SHA-256. The package is unpacked without installing it (no maintainer script runs), and the
licence in its copyright file must be CC0. Writes <out>/lb_LU.aff, <out>/lb_LU.dic and
<out>/VERSION. Standard library only.
"""
import argparse, hashlib, io, lzma, os, sys, tarfile, urllib.request

REPOSITORY = "https://downloads.spellchecker.lu/packages/Ubuntu/"
PACKAGE = "hunspell-lb-lu"
PINNED = ("2.1-2026-06-01", "d5e199b56b1d3e7511c7d90eb455073be6bc6908fc295d4d5b0e38bbba4f09b1")
UA = {"User-Agent": "letzcheck-fetch/1"}
FILES = {"usr/share/myspell/dicts/lb_LU.aff": "lb_LU.aff", "usr/share/myspell/dicts/lb_LU.dic": "lb_LU.dic"}
COPYRIGHT = "usr/share/doc/hunspell-lb-LU/copyright"

def get(url):
    with urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=120) as r:
        return r.read()

def package_record():
    """The repository's Packages stanza for the dictionary, as a dict."""
    for stanza in get(REPOSITORY + "Packages").decode("utf-8").split("\n\n"):
        fields = dict(line.split(": ", 1) for line in stanza.splitlines() if ": " in line)
        if fields.get("Package") == PACKAGE:
            return fields
    sys.exit(f"{PACKAGE} is not in {REPOSITORY}Packages")

def ar_members(data):
    """Members of a Debian package (an ar archive): {name: bytes}."""
    if not data.startswith(b"!<arch>\n"):
        sys.exit("the package is not an ar archive")
    members, at = {}, 8
    while at + 60 <= len(data):
        header = data[at:at + 60]
        name, size = header[:16].decode().strip().rstrip("/"), int(header[48:58].decode().strip())
        members[name] = data[at + 60:at + 60 + size]
        at += 60 + size + (size % 2)
    return members

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--latest", action="store_true", help="the version the repository serves now")
    ap.add_argument("--out", default="data/spell")
    a = ap.parse_args()

    record = package_record()
    version, expected = (record["Version"], record["SHA256"]) if a.latest else PINNED
    if record["Version"] != version:
        sys.exit(f"the repository serves {PACKAGE} {record['Version']}, not the pinned {version}: "
                 "use --latest (and measure the corpus again)")
    data = get(REPOSITORY + record["Filename"].lstrip("./"))
    digest = hashlib.sha256(data).hexdigest()
    if digest != expected:
        sys.exit(f"{PACKAGE} {version}: SHA-256 {digest}, expected {expected}; stopping")

    members = ar_members(data)
    name = next((n for n in members if n.startswith("data.tar")), None)
    if name is None:
        sys.exit("no data.tar in the package")
    raw = members[name]
    if name.endswith(".xz"):
        raw = lzma.decompress(raw)
    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:*") as tar:
        files = {m.name.lstrip("./"): m for m in tar.getmembers() if m.isfile()}
        def read(path):
            if path not in files:
                sys.exit(f"{path} is not in the package")
            return tar.extractfile(files[path]).read()
        licence = read(COPYRIGHT).decode("utf-8")
        if "CC0" not in licence or "public domain" not in licence:
            sys.exit("the package's copyright file no longer says CC0 / public domain; stopping")
        os.makedirs(a.out, exist_ok=True)
        for path, target in FILES.items():
            with open(os.path.join(a.out, target), "wb") as f:
                f.write(read(path))
    with open(os.path.join(a.out, "VERSION"), "w") as f:
        f.write(version + "\n")
    print(f"{PACKAGE} {version} -> {a.out}", file=sys.stderr)

if __name__ == "__main__":
    main()
