// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Regression gate: a new build against the build it replaces, item by item. Exits 1 when the new
// build is worse anywhere it can be measured without a human:
//   corpus  every sentence of the corpus is correct: any remark the base build did not make on a
//           sentence is a new false alarm, and fails.
//   certified mistakes  LOD sentences with one mistake the rule leaves no doubt about
//           (test/mutations.mjs): fails on any case caught or fixed before and not now.
//   typos   words of the corpus with one seeded typo: fails when fewer are caught, fewer get the
//           right word as first fix, or fewer get it among the first four. Each word that got worse
//           is listed, even when the totals hold.
// Remarks the new build no longer makes on the corpus are listed as gains.
// Usage: node test/regress.mjs <base.wasm[@lod.txt]> <new.wasm> <lod.txt> <corpus.txt> [--typos 3000]
import fs from 'node:fs'
import { load, remarks, applyFirstFixes } from './engine.mjs'
import { cases, score as mistakeScore } from './mutations.mjs'

const args = process.argv.slice(2)
const ti = args.indexOf('--typos'), typoCount = ti < 0 ? 3000 : +args.splice(ti, 2)[1]
const [baseSpec, newSpec, lod, corpusFile] = args
if (!corpusFile) { console.error('usage: node test/regress.mjs base.wasm[@lod.txt] new.wasm lod.txt corpus.txt [--typos n]'); process.exit(2) }
const base = await load(baseSpec, lod), next = await load(newSpec, lod)
const corpus = fs.readFileSync(corpusFile, 'utf8').split('\n').filter(Boolean)
const failures = []
const LIMIT = 15
const list = (title, items) => {
  if (!items.length) return
  console.log(`\n${title}: ${items.length}`)
  for (const s of items.slice(0, LIMIT)) console.log('  ' + s)
  if (items.length > LIMIT) console.log(`  … and ${items.length - LIMIT} more`)
}

// ---- corpus ----
const key = r => r.word + '|' + r.rule
const describe = (s, r) => `${s.slice(r.start, r.end)}  ${r.rule} → ${JSON.stringify(r.fixes.slice(0, 2))}\n      in: ${s}`
const added = [], lost = []
for (const s of corpus) {
  const a = remarks(base(s)), b = remarks(next(s))
  const ak = new Set(a.map(key)), bk = new Set(b.map(key))
  for (const r of b) if (!ak.has(key(r))) added.push(describe(s, r))
  for (const r of a) if (!bk.has(key(r))) lost.push(describe(s, r))
}
list('NEW false alarms on correct sentences', added)
list('false alarms gone', lost)
if (added.length) failures.push(`${added.length} new false alarm(s) on the corpus`)

// ---- certified mistakes (test/mutations.mjs) ----
// LOD sentences with one mistake the rule leaves no doubt about; the LOD sentence is the answer.
// Every case the base build fixed (or caught) and the new one does not is a regression.
{
  const levels = ['missed', 'caught', 'fixed']
  const accepted = new Set(fs.readFileSync(new URL('accepted.txt', import.meta.url), 'utf8').split('\n').filter(l => l.trim() && !l.startsWith('#')))
  const acceptedCases = []
  const byFamily = {}, worseCases = []
  for (const c of cases(lod, corpusFile)) {
    const f = byFamily[c.family] ??= { cases: 0, base: [0, 0], next: [0, 0] }
    const a = mistakeScore(base, c, remarks, applyFirstFixes), b = mistakeScore(next, c, remarks, applyFirstFixes)
    f.cases++
    if (a >= 1) f.base[0]++; if (a >= 2) f.base[1]++
    if (b >= 1) f.next[0]++; if (b >= 2) f.next[1]++
    if (b < a) (accepted.has(c.wrong) ? acceptedCases : worseCases).push(`${c.family}: ${levels[a]} → ${levels[b]}\n      ${c.wrong}\n      want: ${c.right}`)
  }
  console.log('\ncertified mistakes:          base → new')
  for (const [family, f] of Object.entries(byFamily)) {
    const p = n => (100 * n / f.cases).toFixed(2) + '%'
    console.log(`  ${(family + ' (' + f.cases + ')').padEnd(26)}   caught ${p(f.base[0])} → ${p(f.next[0])}   fixed ${p(f.base[1])} → ${p(f.next[1])}`)
  }
  list('certified mistakes handled worse than before', worseCases)
  list('handled worse, accepted in test/accepted.txt', acceptedCases)
  if (worseCases.length) failures.push(`${worseCases.length} certified mistake(s) handled worse`)
}

// ---- typos ----
let seed = 11
const rand = n => { seed = (seed * 1103515245 + 12345) & 0x7fffffff; return seed % n }
const LETTERS = 'aäbcdeéëfghijklmnopqrstuüvwxyz'
function typo(w) {
  const i = 1 + rand(w.length - 2), kind = rand(4)
  if (kind === 0) return w.slice(0, i) + w.slice(i + 1)
  if (kind === 1) return w.slice(0, i) + w[i + 1] + w[i] + w.slice(i + 2)
  if (kind === 2) return w.slice(0, i) + LETTERS[rand(LETTERS.length)] + w.slice(i + 1)
  return w.slice(0, i) + LETTERS[rand(LETTERS.length)] + w.slice(i)
}
// Score of one build on one typo: 0 missed, 1 caught, 2 right word among the first four, 3 first.
const score = (run, c) => {
  const r = remarks(run(c.line)).find(r => r.start === c.start)
  if (!r) return 0
  return r.fixes[0] === c.good ? 3 : r.fixes.slice(0, 4).includes(c.good) ? 2 : 1
}
const totals = [[0, 0, 0], [0, 0, 0]], worse = []
let words = 0, tries = 0
while (words < typoCount && tries++ < typoCount * 20) {
  const s = corpus[rand(corpus.length)], found = [...s.matchAll(/\p{L}{5,}/gu)]
  if (!found.length) continue
  const m = found[rand(found.length)], bad = typo(m[0])
  if (bad === m[0]) continue
  // Only words both builds accept as written.
  const accepted = run => !remarks(run(s)).some(r => r.start === m.index)
  if (!accepted(base) || !accepted(next)) continue
  const c = { line: s.slice(0, m.index) + bad + s.slice(m.index + m[0].length), start: m.index, good: m[0], bad }
  const a = score(base, c), b = score(next, c)
  words++
  for (const [t, sc] of [[0, a], [1, b]]) { if (sc >= 1) totals[t][0]++; if (sc >= 2) totals[t][1]++; if (sc >= 3) totals[t][2]++ }
  if (b < a) worse.push(`${bad} (for ${m[0]}): ${['missed', 'caught', 'in the first four', 'first'][a]} → ${['missed', 'caught', 'in the first four', 'first'][b]}`)
}
const pct = n => (100 * n / words).toFixed(1) + '%'
console.log(`\ntypos (${words} words):     base → new`)
const names = ['caught', 'right fix in the first four', 'right fix first']
names.forEach((name, i) => {
  const [a, b] = [totals[0][i], totals[1][i]]
  console.log(`  ${name.padEnd(28)} ${pct(a)} → ${pct(b)}${b < a ? '   WORSE' : b > a ? '   better' : ''}`)
  if (b < a) failures.push(`typos: ${name} ${pct(a)} → ${pct(b)}`)
})
list('typos handled worse than before', worse)

console.log(failures.length ? '\nREGRESSION:\n  ' + failures.join('\n  ') : '\nno regression')
process.exit(failures.length ? 1 : 0)
