// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

// For changes that must not change behaviour (refactoring, renaming, performance): compares the
// full output of two builds (analyze JSON, then describe / gloss / nfull / clitic / level of every
// word, with the French and English glosses) on the corpus sentences and on random texts made from
// their words. Exits 1 on any difference.
// Usage: node test/same-output.mjs <base.wasm> <new.wasm> <lod.txt> <corpus.txt> [random texts, default 40000]
import fs from 'fs'
const [A, B, lod, corpus, n = '40000'] = process.argv.slice(2)
// A build is "engine.wasm" (reads lod.txt) or "engine.wasm@other-lod.txt" (an older build with
// the data file of its own format).
async function load(spec) {
  const [f, own] = spec.split('@'), data = fs.readFileSync(own || lod)
  const { instance } = await WebAssembly.instantiate(fs.readFileSync(f), {}); const x = instance.exports
  const p = x.alloc(data.length); new Uint8Array(x.memory.buffer).set(data, p); x.init(p, data.length)
  const rd = q => new TextDecoder().decode(new Uint8Array(x.memory.buffer, q + 4, new DataView(x.memory.buffer).getUint32(q, true)))
  return (s, lang) => {
    const w = new TextEncoder().encode(s); const q = x.alloc(w.length); new Uint8Array(x.memory.buffer).set(w, q)
    let out = rd(x.analyze(q, w.length)); x.dealloc(q, w.length)
    const nw = (JSON.parse(out).w || []).length
    for (let i = 0; i < nw + 1; i++) out += rd(x.describe(i)) + rd(x.gloss(i, lang & 1)) + rd(x.nfull(i)) + rd(x.clitic(i)) + x.level(i)
    return out
  }
}
const fa = await load(A), fb = await load(B)
const sents = fs.readFileSync(corpus, 'utf8').split('\n').filter(Boolean)
const words = sents.join(' ').split(/\s+/)
let seed = 12345; const rnd = k => { seed = (seed * 1103515245 + 12345) & 0x7fffffff; return seed % k }
const mut = w => {
  switch (rnd(14)) {
    case 0: return w.toLowerCase(); case 1: return w[0].toUpperCase() + w.slice(1); case 2: return w.toUpperCase()
    case 3: return w.replace(/[äëéèê]/, 'e'); case 4: return w + 'n'; case 5: return w.replace(/n$/, '')
    case 6: return w.slice(0, -1); case 7: return w + w.slice(-1); case 8: return String(rnd(30))
    case 9: return w.replace("'", '’'); default: return w
  }
}
const seps = [' ', ' ', ' ', ' ', '  ', ', ', ' , ', '. ', ' . ', '! ', ' ? ', ': ', '\n', '… ', ' - ', '-', "'", ' "', '" ', ' (', ') ', '. . . ', '3 ', ' 2,5 ', ' 😀 ', ' z. B. ']
let diff = 0
const texts = sents.slice(0, 2000).concat(sents.slice(0, 2000).map(s => s.toLowerCase()))
for (let i = 0; i < +n; i++) {
  let t = ''; const len = 1 + rnd(14)
  for (let j = 0; j < len; j++) t += mut(words[rnd(words.length)] || 'a') + seps[rnd(seps.length)]
  texts.push(t)
}
for (const [i, t] of texts.entries()) {
  for (const lang of [0, 1]) {
    if (fa(t, lang) !== fb(t, lang)) { if (diff++ < 5) console.log('DIFF', lang, JSON.stringify(t)) }
  }
}
console.log(texts.length, 'texts x 2 gloss languages,', diff, 'differences')
process.exit(diff ? 1 : 0)
