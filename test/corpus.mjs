// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

import fs from 'fs'
const [oldW, newW, lod, corpus] = process.argv.slice(2).filter(a => a !== '--strict')
// A build is "engine.wasm" (reads lod.txt) or "engine.wasm@other-lod.txt" (an older build with
// the data file of its own format).
async function engine(spec) {
  const [path, own] = spec.split('@'), data = fs.readFileSync(own || lod)
  const { instance } = await WebAssembly.instantiate(fs.readFileSync(path), {})
  const x = instance.exports, enc = new TextEncoder(), dec = new TextDecoder()
  const mem = () => new Uint8Array(x.memory.buffer)
  const out = p => { const len = new DataView(x.memory.buffer).getUint32(p, true); return dec.decode(new Uint8Array(x.memory.buffer, p + 4, len)) }
  const p = x.alloc(data.length); mem().set(data, p); if (x.init(p, data.length)) throw new Error('init')
  return s => { const cap = s.length * 3 || 1, ptr = x.alloc(cap); const n = enc.encodeInto(s, mem().subarray(ptr, ptr + cap)).written; try { return JSON.parse(out(x.analyze(ptr, n))) } finally { x.dealloc(ptr, cap) } }
}
// oldW '-' compares against nothing: every remark on the corpus is listed (the corpus is all correct sentences).
const A = oldW === '-' ? () => ({ i: [] }) : await engine(oldW), B = await engine(newW)
const lines = fs.readFileSync(corpus, 'utf8').split('\n').filter(Boolean)
// Remarks are [word, severity, category, rule, params, fixes, end?] (an older data layout put the
// fixes fifth, with no parameters).
const norm = i => Array.isArray(i[4])
  ? { wi: i[0], sev: i[1], cat: i[2], what: null, text: String(i[3]), fixes: i[4] }
  : { wi: i[0], sev: i[1], cat: i[2], what: i[3] + ' ' + JSON.stringify(i[4]), text: i[3] + ' ' + JSON.stringify(i[4]), rule: i[3], fixes: i[5] }
const key = i => i.wi + '|' + i.sev + '|' + i.cat
// Same remark, other content: compared on the rule and parameters when both builds give them,
// on the fixes otherwise.
const differs = (a, b) => a.what !== null && b.what !== null ? a.what !== b.what : JSON.stringify(a.fixes) !== JSON.stringify(b.fixes)
const fp = new Map(); let lost = 0, lostEx = [], added = 0, reworded = 0
// What a group of remarks is about: its rule.
const group = i => i.rule || i.text.slice(0, 60)
for (const s of lines) {
  const a = A(s).i.map(norm), b = B(s).i.map(norm)
  const bk = new Map(b.map(i => [key(i), i]))
  for (const i of a) if (!bk.has(key(i))) { lost++; if (lostEx.length < 40) lostEx.push([s, i]) }
  const ak = new Map(a.map(i => [key(i), i]))
  for (const i of b) {
    // A remark on a word the base build already flagged at least as severely is not a new alarm,
    // only a milder or different one: listed as reworded.
    const rank = { err: 3, warn: 2, info: 1 }
    const milder = a.some(x => x.wi === i.wi && rank[x.sev] >= rank[i.sev])
    const changed = ak.has(key(i)) ? (differs(ak.get(key(i)), i) ? 'msg' : null) : (milder ? 'msg' : 'new')
    if (!changed) continue
    if (changed === 'new') added++; else reworded++
    const r = (changed === 'msg' ? '[changed] ' : '') + i.cat + ': ' + group(i)
    if (!fp.has(r)) fp.set(r, { n: 0, ex: [] })
    const e = fp.get(r); e.n++
    if (e.ex.length < 6) e.ex.push(s + '   ⟶ ' + i.text + ' ' + JSON.stringify(i.fixes))
  }
}
console.log('sentences', lines.length, '| old issues lost:', lost, lostEx.map(x => x[0] + ' :: ' + x[1].text).join('\n'))
for (const [r, e] of [...fp].sort((a, b) => b[1].n - a[1].n)) { console.log(`\n${e.n}\t${r}`); for (const x of e.ex) console.log('     ', x) }
console.log(`\n${added} remark(s) on correct sentences` + (oldW === '-' ? '' : ' that the base build does not make') + `, ${reworded} reworded, ${lost} lost`)
// --strict (used by CI): a remark the base build does not make is a false alarm until shown otherwise.
// A remark the base build made too, with another message ([changed]), is listed for review only.
if (process.argv.includes('--strict') && added > 0) process.exit(1)
