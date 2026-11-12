// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Shared by the test scripts: load a build, analyse a text, apply fixes.
import fs from 'node:fs'

// A build is "engine.wasm" (reads `lod`) or "engine.wasm@other-lod.txt" (an older build with the
// data file of its own format).
export async function load(spec, lod) {
  const [path, own] = spec.split('@'), data = fs.readFileSync(own || lod)
  const { instance } = await WebAssembly.instantiate(fs.readFileSync(path), {})
  const x = instance.exports, enc = new TextEncoder(), dec = new TextDecoder()
  const mem = () => new Uint8Array(x.memory.buffer)
  const out = p => { const len = new DataView(x.memory.buffer).getUint32(p, true); return dec.decode(new Uint8Array(x.memory.buffer, p + 4, len)) }
  const p = x.alloc(data.length); mem().set(data, p)
  if (x.init(p, data.length)) throw new Error('data file rejected by ' + path)
  return text => {
    const cap = text.length * 3 || 1, ptr = x.alloc(cap)
    const n = enc.encodeInto(text, mem().subarray(ptr, ptr + cap)).written
    try { return JSON.parse(out(x.analyze(ptr, n))) } finally { x.dealloc(ptr, cap) }
  }
}

// Remarks as objects. A raw remark is [word, severity, category, rule, params, fixes, end?] (an
// older data layout put the fixes fifth, with no rule parameters). Offsets are UTF-16, as in JS.
export function remarks(result) {
  const words = []; let start = 0
  for (let t = 0; t < result.k.length; t++) { if (result.k[t] === 'w') words.push([start, result.e[t]]); start = result.e[t] }
  return result.i.map(i => {
    const old = Array.isArray(i[4])
    return {
      word: i[0], start: words[i[0]][0], end: (!old && i[6] != null) ? i[6] : words[i[0]][1],
      severity: i[1], category: i[2], rule: old ? String(i[3]) : i[3],
      params: old ? {} : i[4], fixes: old ? i[4] : i[5],
    }
  })
}

// The text once the first fix of every remark is applied, as a user clicking each one would get
// it: a fix spanning several words wins over the ones inside it, an elided article ("d'") gets
// its space back when replaced by a full one, a removed word takes its space with it.
export function applyFirstFixes(text, list) {
  const edits = list.filter(r => r.fixes.length).map(r => ({ s: r.start, e: r.end, f: r.fixes[0] }))
  let floor = Infinity
  for (const x of edits.sort((a, b) => b.s - a.s)) {
    if (x.e > floor) continue
    floor = x.s
    let { s, e, f } = x
    const origElided = /['’´`ʼ‘]$/.test(text.slice(s, e)), newElided = /'$/.test(f)
    if (origElided && !newElided) f += ' '
    if (!origElided && newElided && text[e] === ' ') e++
    if (f === '' && text[s - 1] === ' ') s--
    text = text.slice(0, s) + f + text.slice(e)
  }
  return text
}
