// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Minimal use of the engine from Node, without any glue library.
// Usage: node examples/check.mjs "Ech hunn ee Fro."   (after `make`)
//
// The engine returns structured remarks (a category, a rule key, named parameters, fixes). Turning
// them into sentences, in whatever language, is up to the product that uses it; this example just
// prints them.
import fs from 'node:fs'

const text = process.argv[2] ?? 'Ech hunn ee Fro.'

const { instance } = await WebAssembly.instantiate(fs.readFileSync('dist/letz.wasm'), {})
const x = instance.exports
const bytes = () => new Uint8Array(x.memory.buffer)
// Every *out* pointer is a u32 little-endian length followed by UTF-8, valid until the next call.
const read = p => new TextDecoder().decode(bytes().subarray(p + 4, p + 4 + new DataView(x.memory.buffer).getUint32(p, true)))

// The dictionary: the module takes ownership of this buffer.
const lod = fs.readFileSync('data/lod.txt')
const p = x.alloc(lod.length)
bytes().set(lod, p)
if (x.init(p, lod.length) !== 0) throw new Error('lod.txt rejected')

const input = new TextEncoder().encode(text)
const q = x.alloc(input.length)
bytes().set(input, q)
const r = JSON.parse(read(x.analyze(q, input.length)))
x.dealloc(q, input.length)

// Word wi is the wi-th 'w' in r.k; r.e holds the UTF-16 end offset of every token.
const words = []
let start = 0
for (let t = 0; t < r.k.length; t++) { if (r.k[t] === 'w') words.push([start, r.e[t]]); start = r.e[t] }
for (const [wi, sev, category, rule, params, fixes] of r.i) {
  console.log(`${sev.padEnd(4)} ${text.slice(...words[wi]).padEnd(8)} ${category}/${rule} ${JSON.stringify(params)}  ${fixes.length ? '→ ' + fixes.join(' / ') : ''}`)
}
if (!r.i.length) console.log('no remark')
