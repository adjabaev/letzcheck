# This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
# If a copy of the MPL was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Builds the engine's dictionary index (lod.json) from the LOD open data (CC0, data.public.lu).
Usage: python3 tools/build_lod.py data/lod data/lod.json
The input folder is the one tools/fetch_lod.py fills: art/, tab/, search/ and VERSION (YYMMDD)."""
import collections, json, re, sys
import xml.etree.ElementTree as ET
D, OUT = sys.argv[1].rstrip("/") + "/", sys.argv[2]
L = "A-Za-zÀ-ÖØ-öø-ÿ"
WORD = re.compile(rf"^[{L}][{L}'’\-]*$")
def clean(t): return re.sub(r"\s+", " ", (t or "")).strip()

# 1. Articles: translations, plural forms
info = {}
for ev, el in ET.iterparse(D + "art/new_lod-art.xml"):
    if el.tag != "entry": continue
    plural, tr = [], {"fr": [], "en": [], "de": []}
    ms = el.find("microStructure")
    if ms is not None:
        for inf in ms.findall("inflection"):
            if inf.get("type") == "plural":
                plural = [clean(f.text) for f in inf.findall("form") if clean(f.text)]
    for m in el.iter("meaning"):
        for tl in m.findall("targetLanguage"):
            lg, t = tl.get("lang"), tl.find("translation")
            if lg in tr and t is not None and len(tr[lg]) < 3:
                t = clean("".join(t.itertext()))
                if t and t not in tr[lg]: tr[lg].append(t)
    info[el.get("id")] = (plural[:3], tr, clean(el.findtext("ipa")))
    el.clear()

# 2. Verb tables
verbs = {}
for ev, el in ET.iterparse(D + "tab/new_lod-tab.xml"):
    if el.tag != "verbConjugation": continue
    def tense(path): return [clean(el.findtext(f"{path}/p{i}")) for i in range(1, 7)]
    verbs[el.get("id")] = dict(inf=clean(el.findtext("infinitive")), pp=clean(el.findtext("pastParticiple")),
        aux=clean(el.findtext("auxiliaryVerb")), pres=tense("indicative/present"),
        past=tense("indicative/pastSimple"), cond=tense("conditional/presentSimple"))
    el.clear()

# How a word starting with Y sounds, from its IPA: "j" when the Y is the consonant [j] (Yoga:
# the n drops before it), "v" when it is a vowel (Yttrium: the n stays), "" otherwise.
def y_onset(lemma, ipa):
    if not lemma[:1] in ("y", "Y") or not ipa: return ""
    sound = ipa.lstrip("ˈˌ.( ")
    if sound.startswith("j"): return "j"
    if sound[:1] in "aeiouyɛɪʏøœɔʊəɐæɑɒ": return "v"
    return ""

# 3. Search index: every spelling of every entry
entries, eidx = [], {}
ok = collections.defaultdict(set)        # form -> entries where it is a valid spelling
indep = collections.defaultdict(set)     # form -> entries where it stands on its own (not an n-rule variant)
nfull = collections.defaultdict(set)     # n-rule form -> full form(s)
wrong = collections.defaultdict(set)     # known misspelling -> entries
attested = collections.Counter()
espell = {}
VALID = {None, "n-rule", "filled-croissant", "adj-to-Subst-es", "PPresent", "allegro", "e_i-i_e"}
for ev, el in ET.iterparse(D + "search/new_lod-search.xml"):
    if el.tag != "entry": continue
    eid, lemma = el.get("id"), clean(el.findtext("lemma"))
    p = el.find("partOfSpeech")
    pos = clean(p.text) if p is not None and p.text else ""
    gen = (p.get("gen") or "") if p is not None else ""
    cats = {c.text for c in el.findall("categories/category")}
    lvl = 1 if cats & {"A1", "GWS A1"} else 2 if cats & {"A2", "GWS A2"} else 0
    plural, tr, ipa = info.get(eid, ([], {"fr": [], "en": [], "de": []}, ""))
    i = len(entries); eidx[eid] = i
    entries.append([eid, lemma, pos, gen, lvl, "; ".join(tr["fr"]), "; ".join(tr["en"]), ", ".join(plural), y_onset(lemma, ipa)])
    sp = [(clean(s.text).replace("’", "'"), s.get("reason")) for s in el.findall("spellings/spelling")]
    single = [(t, r) for t, r in sp if t and " " not in t and WORD.match(t)]
    valid = {t for t, r in single if r in VALID}
    espell[i] = valid
    multi = " " in lemma
    for t, r in single:
        if r == "erroneous-spelling": wrong[t].add(i); continue
        if r not in VALID: continue
        ok[t].add(i)
        full = next((f for f in (t + "n", t + "nn") if f in valid and f != t), None)
        is_n = bool(full) and (r == "n-rule" or full == lemma or (pos == "VRB" and t != lemma))
        if is_n: nfull[t].add(full)
        elif not multi and pos != "INTERJ" and len(lemma) > 1: indep[t].add(i)
    for tx in el.iter("text"):
        for w in re.findall(rf"[{L}][{L}'’\-]*", clean("".join(tx.itertext()))):
            attested[w.replace("’", "'")] += 1
    el.clear()

# 4. Verb person masks (finite forms) and "wild" verb forms (infinitive, participle…) never flagged
mask, wild, vout = collections.Counter(), set(), {}
for vid, v in verbs.items():
    i = eidx.get(vid)
    if i is None: continue
    finite = set()
    for tns in (v["pres"], v["past"], v["cond"]):
        for b, cell in enumerate(tns):
            for alt in cell.split(" / "):
                w = alt.split(" ")[0] if alt else ""
                if not w: continue
                for f in {w, w + "n", w + "nn"} & (espell[i] | {w}):
                    mask[f] |= 1 << b; finite.add(f)
    for f in espell[i] - finite: wild.add(f)
    for f in re.split(r" / | ", v["inf"] + " " + v["pp"]):
        if f: wild.add(f)
    vout[i] = [v["aux"], v["pp"]] + v["pres"]
for f, fulls in nfull.items():
    for full in fulls:
        if full in mask: mask[f] |= mask[full]
        if full in wild: wild.add(f)

# 5. Emit
def rank(form, i):
    e = entries[i]
    return (" " in e[1], e[1] != form, e[1].lower() != form.lower(), e[4] == 0, e[4], i)
lines = []
for f in sorted(set(ok) | set(wrong)):
    if f in ok:
        refs = sorted(ok[f], key=lambda i: rank(f, i))
        if any(" " not in entries[i][1] for i in refs): refs = [i for i in refs if " " not in entries[i][1]]
        flag = ("n" if f in nfull and not indep[f] else "")
        lines.append("\t".join([f, ",".join(map(str, refs[:5])), "|".join(sorted(nfull.get(f, ()))), flag]))
    else:
        lines.append("\t".join([f, "!" + ",".join(map(str, sorted(wrong[f])[:3])), "", ""]))
att = sorted(w for w, c in attested.items() if w not in ok and c >= 2 and len(w) <= 30)
plines = [f"{f}\t{m}{'w' if f in wild else ''}" for f, m in sorted(mask.items())]
ver = open(D + "VERSION").read().strip()
data = {"v": f"20{ver[:2]}-{ver[2:4]}-{ver[4:6]}", "e": entries, "f": "\n".join(lines), "a": "\n".join(att), "p": "\n".join(plines), "c": vout}
s = json.dumps(data, ensure_ascii=False, separators=(",", ":"))
open(OUT, "w", encoding="utf8").write(s)
print(f"entries {len(entries)} forms {len(lines)} verbs {len(vout)} bytes {len(s.encode())}", file=sys.stderr)
