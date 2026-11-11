// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Hand-written cases. Exits 1 when one fails.
//   test/new-rules.txt  one sentence per line, each must get at least one remark
//   test/no-alarm.txt   correct sentences, none may get a remark
//   test/known.txt      known gaps, reported but not failing: "+ " should get a remark and does
//                       not yet, "- " should get none and does. A gap that closes is reported so the
//                       line can move to one of the two files above.
// Usage: node test/cases.mjs <letz.wasm> <lod.txt> [--quiet]
import fs from 'fs'
const [W, D, flag] = process.argv.slice(2)
const quiet = flag === '--quiet'
const here = f => new URL(f, import.meta.url)
const data = fs.readFileSync(D)
const { instance } = await WebAssembly.instantiate(fs.readFileSync(W), {})
const x = instance.exports, enc = new TextEncoder(), dec = new TextDecoder(), mem = () => new Uint8Array(x.memory.buffer)
const out = p => { const l = new DataView(x.memory.buffer).getUint32(p, true); return dec.decode(new Uint8Array(x.memory.buffer, p + 4, l)) }
const p = x.alloc(data.length); mem().set(data, p); if (x.init(p, data.length)) throw new Error('lod.txt rejected')
const run = s => { const cap = s.length * 3 || 1, ptr = x.alloc(cap); const n = enc.encodeInto(s, mem().subarray(ptr, ptr + cap)).written; try { return JSON.parse(out(x.analyze(ptr, n))) } finally { x.dealloc(ptr, cap) } }
const lines = f => fs.readFileSync(here(f), 'utf8').split('\n').filter(l => l.trim() && !l.startsWith('#'))
// A remark is [word, severity, category, rule, params, fixes, end?].
const show = (s, r) => { console.log((r.i.length ? '  ' : '∅ ') + s); for (const i of r.i) console.log('      [' + i[1] + '] ' + i[2] + '/' + i[3] + ' ' + JSON.stringify(i[4]) + ' → ' + JSON.stringify(i[5]) + (i[6] != null ? ' end=' + i[6] : '')) }

const failures = [], closed = []
for (const [file, wantRemark] of [['new-rules.txt', true], ['no-alarm.txt', false]]) {
  if (!quiet) console.log('== test/' + file)
  for (const s of lines(file)) {
    const r = run(s)
    if (!quiet) show(s, r)
    if ((r.i.length > 0) !== wantRemark) failures.push(`${file}: ${s}` + (wantRemark ? ' (no remark)' : ` (${r.i.map(i => i[2]).join(', ')})`))
  }
}
for (const l of lines('known.txt')) {
  const want = l[0] === '+', s = l.slice(2), r = run(s)
  if ((r.i.length > 0) === want) closed.push(l)
}
console.log(`\n${failures.length} failing case(s)` + failures.map(f => '\n  ' + f).join(''))
if (closed.length) console.log(`known gaps now closed, move them out of test/known.txt:` + closed.map(f => '\n  ' + f).join(''))
process.exit(failures.length ? 1 : 0)
