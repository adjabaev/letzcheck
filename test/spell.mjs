// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

// The engine's reader of the spellchecker.lu dictionary (src/spell.rs) against the real hunspell:
// every word of the corpus, every dictionary word and typo variants of corpus words must get the
// same verdict. Needs the hunspell program. Exits 1 on any difference.
// Usage: node test/spell.mjs <letz.wasm> <lod.txt> <corpus.txt> <dir with lb_LU.aff/.dic> [typos, default 30000] [--json]
import fs from 'node:fs'
import { execFileSync } from 'node:child_process'

const json = process.argv.includes('--json')
const [wasm, lod, corpus, spellDir, typoCount = '30000'] = process.argv.slice(2).filter(a => a !== '--json')
const { instance } = await WebAssembly.instantiate(fs.readFileSync(wasm), {})
const x = instance.exports, enc = new TextEncoder()
const mem = () => new Uint8Array(x.memory.buffer)
const data = fs.readFileSync(lod)
const p = x.alloc(data.length); mem().set(data, p)
if (x.init(p, data.length)) throw new Error('init')
const engine = word => {
  const bytes = enc.encode(word), q = x.alloc(bytes.length || 1)
  mem().set(bytes, q)
  const ok = x.spell(q, bytes.length) === 1
  x.dealloc(q, bytes.length || 1)
  return ok
}

// Words: corpus tokens, dictionary words, typos of corpus words.
const words = new Set()
const text = fs.readFileSync(corpus, 'utf8')
for (const m of text.matchAll(/[\p{L}][\p{L}'’-]*/gu)) words.add(m[0].replace(/’/g, "'"))
for (const line of fs.readFileSync(`${spellDir}/lb_LU.dic`, 'utf8').split('\n').slice(1)) {
  const word = line.trim().split(/\s+/)[0].split('/')[0]
  if (word) words.add(word)
}
let seed = 11
const rand = n => { seed = (seed * 1103515245 + 12345) & 0x7fffffff; return seed % n }
const pool = [...words].filter(w => w.length > 3)
const LETTERS = 'aäbcdeéëfghijklmnopqrstuüvwxyz'
for (let n = 0; n < +typoCount; n++) {
  const w = pool[rand(pool.length)], i = rand(w.length)
  const kind = rand(4)
  words.add(kind === 0 ? w.slice(0, i) + w.slice(i + 1)
    : kind === 1 ? w.slice(0, i) + (w[i + 1] || '') + w[i] + w.slice(i + 2)
    : kind === 2 ? w.slice(0, i) + LETTERS[rand(LETTERS.length)] + w.slice(i + 1)
    : w.slice(0, i) + LETTERS[rand(LETTERS.length)] + w.slice(i))
}
// Hyphens and apostrophes make hunspell split the word (BREAK): keep plain words only.
const list = [...words].filter(w => /^[\p{L}]+$/u.test(w))

// hunspell -G prints the words it accepts; run it on chunks to keep the pipes small.
const accepted = new Set()
for (let i = 0; i < list.length; i += 20000) {
  const chunk = list.slice(i, i + 20000)
  const out = execFileSync('hunspell', ['-d', `${spellDir}/lb_LU`, '-i', 'utf-8', '-G'], { input: chunk.join('\n') + '\n', maxBuffer: 1 << 26 })
  for (const w of out.toString().split('\n')) if (w) accepted.add(w)
}
const onlyHunspell = [], onlyEngine = []
for (const w of list) {
  const h = accepted.has(w), e = engine(w)
  if (h && !e) onlyHunspell.push(w)
  if (e && !h) onlyEngine.push(w)
}
if (json) {
  // For test/bench.mjs: the counts only, exit 0.
  console.log(JSON.stringify({ words: list.length, accepted: accepted.size, differences: onlyHunspell.length + onlyEngine.length }))
  process.exit(0)
}
console.log(`${list.length} words, hunspell accepts ${accepted.size}`)
console.log(`accepted by hunspell only: ${onlyHunspell.length}  ${onlyHunspell.slice(0, 25).join(' ')}`)
console.log(`accepted by the engine only: ${onlyEngine.length}  ${onlyEngine.slice(0, 25).join(' ')}`)
process.exit(onlyHunspell.length || onlyEngine.length ? 1 : 0)
