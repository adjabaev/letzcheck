// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

// lod.json (+ the spellchecker.lu Hunspell dictionary) -> lod.txt: one line-based UTF-8 text
// file the Rust engine reads without a JSON parser.
// Usage: node tools/convert.mjs data/lod.json data/lod.txt [data/spell]
//
// Layout (all lines end with \n, fields are tab-separated):
//   LETZ3 <version> <nE> <nF> <nA> <nP> <nC> <nS> <nD> <spellVersion>
//   nE lines  id lemma pos gender level fr en plural yOnset de pt   (entry index = line order;
//             yOnset: "v" when the n stays before the word's first sound although its first letter
//             says otherwise (Job [dʒ], Yttrium), "j" when it drops (Yoga, User); "" otherwise;
//             fr en de pt: the LOD translations, "; "-separated)
//   nF lines  form ids nfull flag                          (unchanged from lod.json "f")
//   nA lines  attested word                                (unchanged from "a")
//   nP lines  form mask[w]                                 (unchanged from "p")
//   nC lines  entryIndex aux pastParticiple p1 .. p6       (from "c", in the JSON's key order)
//   nS lines  Hunspell affix directives, space-separated as in lb_LU.aff, kept: FLAG CIRCUMFIX
//             NOSUGGEST FULLSTRIP KEY TRY REP MAP PFX SFX (rules without their morphology fields)
//   nD lines  word flags                                   (lb_LU.dic without its morphology)
// Without a spell folder nS and nD are 0 and spellVersion is "-".
//
// The script refuses to convert if the data breaks an assumption the Rust port relies on.
import { existsSync, readFileSync, writeFileSync } from 'node:fs';

const [src, dest, spellDir] = process.argv.slice(2);
if (!src || !dest) { console.error('usage: convert.mjs <lod.json> <lod.txt>'); process.exit(2); }
const d = JSON.parse(readFileSync(src, 'utf8'));
const fail = (m) => { console.error('convert: ' + m); process.exit(1); };
const clean = (s, what) => { if (typeof s !== 'string' || /[\t\n\r]/.test(s)) fail('tab/newline or non-string in ' + what + ': ' + JSON.stringify(s)); return s; };

if (typeof d.v !== 'string' || /\s/.test(d.v)) fail('bad version');
const E = d.e.map((e, i) => {
  if (!Array.isArray(e) || e.length !== 11) fail('entry ' + i + ' is not 11 fields');
  if (!/^[jv]?$/.test(e[8])) fail('entry ' + i + ' y onset ' + e[8]);
  if (!Number.isInteger(e[4]) || e[4] < 0 || e[4] > 9) fail('entry ' + i + ' level');
  if (!/^[MFN]*$/.test(e[3])) fail('entry ' + i + ' gender ' + e[3]);
  return [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10].map((k) => (k === 4 ? String(e[4]) : clean(e[k], 'entry ' + i))).join('\t');
});
const F = d.f.split('\n');
const seen = new Set();
for (const l of F) {
  const a = l.split('\t');
  if (a.length !== 4) fail('form line without 4 fields: ' + l);
  if (!a[0] || /\r/.test(l)) fail('bad form line: ' + JSON.stringify(l));
  if (!/^!?\d+(,\d+)*$/.test(a[1])) fail('ids: ' + l);
  for (const x of a[1].replace('!', '').split(',')) if (!d.e[+x]) fail('id out of range: ' + l);
  if (a[3] !== '' && a[3] !== 'n') fail('flag: ' + l);
  if (seen.has(a[0])) fail('duplicate form: ' + a[0]);
  seen.add(a[0]);
}
const A = d.a.split('\n');
for (const l of A) if (/[\t\r]/.test(l)) fail('attested: ' + JSON.stringify(l));
const P = d.p.split('\n');
const pseen = new Set();
for (const l of P) {
  if (!/^[^\t\r]+\t\d{1,9}w?$/.test(l)) fail('person line: ' + JSON.stringify(l));
  const k = l.split('\t')[0]; if (pseen.has(k)) fail('duplicate person form ' + k); pseen.add(k);
}
const C = Object.entries(d.c).map(([k, v]) => {
  if (!/^(0|[1-9]\d*)$/.test(k) || !d.e[+k]) fail('conjugation key ' + k);
  if (!Array.isArray(v) || v.length !== 8) fail('conjugation ' + k + ' is not 8 fields');
  return k + '\t' + v.map((x) => clean(x, 'conjugation ' + k)).join('\t');
});

// The spellchecker.lu dictionary (optional).
const S = [], D = [];
let spellVersion = '-';
if (spellDir && existsSync(spellDir + '/lb_LU.aff')) {
  spellVersion = readFileSync(spellDir + '/VERSION', 'utf8').trim();
  if (/\s/.test(spellVersion)) fail('spell version');
  const KEEP = new Set(['FLAG', 'CIRCUMFIX', 'NOSUGGEST', 'FULLSTRIP', 'KEY', 'TRY', 'REP', 'MAP', 'PFX', 'SFX']);
  for (const raw of readFileSync(spellDir + '/lb_LU.aff', 'utf8').split(/\r?\n/)) {
    const f = raw.trim().split(/\s+/);
    if (!KEEP.has(f[0])) continue;
    if (f[0] === 'FLAG' && f[1] !== 'long') fail('only FLAG long is supported, not ' + f[1]);
    // a PFX/SFX header has 4 fields (name flag Y/N count), a rule 5 and morphology after them
    const rule = (f[0] === 'PFX' || f[0] === 'SFX') && !/^[YN]$/.test(f[2]);
    S.push((rule ? f.slice(0, 5) : f).join(' '));
  }
  const dic = readFileSync(spellDir + '/lb_LU.dic', 'utf8').split(/\r?\n/).slice(1);
  for (const raw of dic) {
    const field = raw.trim().split(/\s+/)[0];
    if (!field) continue;
    const slash = field.indexOf('/');
    const word = slash < 0 ? field : field.slice(0, slash), flags = slash < 0 ? '' : field.slice(slash + 1);
    if (!word || flags.length % 2 || /[^\x21-\x7e]/.test(flags)) fail('dictionary line: ' + raw);
    D.push(word + '\t' + flags);
  }
}

const head = ['LETZ3', d.v, E.length, F.length, A.length, P.length, C.length, S.length, D.length, spellVersion].join('\t');
const txt = [head, ...E, ...F, ...A, ...P, ...C, ...S, ...D].join('\n') + '\n';
writeFileSync(dest, txt);
console.log(`${dest}: ${Buffer.byteLength(txt)} bytes, version ${d.v}, ${E.length} entries, ${F.length} forms, ${A.length} attested, ${P.length} person masks, ${C.length} conjugations, spellchecker.lu ${spellVersion}: ${S.length} affix lines, ${D.length} words`);
