# This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
# If a copy of the MPL was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Correct sentences for false-positive tests: the example sentences of the LOD articles (CC0).
Usage: python3 tools/lod_examples.py data/lod/art/new_lod-art.xml data/corpus.txt
The XML stores examples as word lists (no punctuation, no initial capital), so each one is rebuilt
as a sentence: words joined by spaces, clitics glued (d' + Auto -> d'Auto), first letter capitalised, final period."""
import random, re, sys
import xml.etree.ElementTree as ET

src, out = sys.argv[1], sys.argv[2]
seen, sents = set(), []
for ev, el in ET.iterparse(src):
    if el.tag != "example": continue
    t = el.find("text")
    if t is not None:
        words = [(w.text or "").strip() for w in t if w.tag in ("word", "inflectedHeadword")]
        words = [w for w in words if w]
        s = ""
        for w in words:
            if not s: s = w
            elif w in (",", ".", "!", "?", ";", ":"): s += w
            elif s.endswith(("'", "’")) and not s.endswith(" '"): s += w
            elif w in ("'s", "'t", "'n", "s"): s += " " + w if w == "s" else w
            else: s += " " + w
        if len(words) >= 3 and not re.search(r"[0-9…/]|\.\.\.|\b(sth|sb)\b", s):
            s = s[0].upper() + s[1:] + ("" if s[-1] in ".!?" else ".")
            if s not in seen: seen.add(s); sents.append(s)
    el.clear()
random.Random(7).shuffle(sents)
open(out, "w").write("\n".join(sents) + "\n")
print(len(sents), "sentences")
