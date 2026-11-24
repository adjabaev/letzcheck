// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Quality bench: one or more engine builds, and the spellchecker.lu dictionary run by the real
// hunspell program as the reference, on the same tests. Writes JSON (stdout or --out).
//
//   false alarms  every sentence of the corpus is correct: a sentence with a remark is a false alarm
//   typos         words of the corpus with one typo (deletion, swap, wrong letter, extra letter),
//                 seeded: is it caught, is the right word the first fix, is it among the first four
//   grammar       the wrong sentences of test/new-rules.txt that are not spelling mistakes (every
//                 word exists; the mistake is the n-rule, gender, agreement…)
//   certified     LOD sentences with one certain mistake (test/mutations.mjs), last build only:
//                 caught, and fixed back to the LOD sentence by the first fix
//   speed         time to analyse the whole corpus with the last build
//   speller       src/spell.rs against hunspell, word by word (as test/spell.mjs)
//
// Usage: node test/bench.mjs [--out bench.json] [--typos 2000] <label=engine.wasm[@lod.txt]>...
// Builds without "@" read data/lod.txt; the last build is the one measured for speed. Needs
// hunspell (apt install hunspell) and `make data`.
import fs from 'node:fs'
import { execFileSync } from 'node:child_process'

const args = process.argv.slice(2)
const option = (name, fallback) => { const i = args.indexOf(name); if (i < 0) return fallback; const v = args[i + 1]; args.splice(i, 2); return v }
const outFile = option('--out', null), typoCount = +option('--typos', '2000')
const specs = args.map(a => { const [label, rest] = a.split('='); const [wasm, lod] = rest.split('@'); return { label, wasm, lod: lod || 'data/lod.txt' } })
if (!specs.length) { console.error('usage: node test/bench.mjs [--out f] [--typos n] label=engine.wasm[@lod.txt]...'); process.exit(2) }
const DICT = 'data/spell/lb_LU'
const lodHead = fs.readFileSync('data/lod.txt', 'utf8').slice(0, 200).split('\n')[0].split('\t')

async function engine({ wasm, lod }) {
  const { instance } = await WebAssembly.instantiate(fs.readFileSync(wasm), {}); const x = instance.exports
  const mem = () => new Uint8Array(x.memory.buffer), enc = new TextEncoder(), dec = new TextDecoder()
  const out = p => dec.decode(new Uint8Array(x.memory.buffer, p + 4, new DataView(x.memory.buffer).getUint32(p, true)))
  const data = fs.readFileSync(lod); const p = x.alloc(data.length); mem().set(data, p)
  if (x.init(p, data.length)) throw new Error('init ' + wasm)
  return text => {
    const cap = text.length * 3 || 1, ptr = x.alloc(cap)
    const n = enc.encodeInto(text, mem().subarray(ptr, ptr + cap)).written
    const r = JSON.parse(out(x.analyze(ptr, n))); x.dealloc(ptr, cap)
    const words = []; let s = 0
    for (let t = 0; t < r.k.length; t++) { if (r.k[t] === 'w') words.push([s, r.e[t]]); s = r.e[t] }
    // [word, severity, category, rule, params, fixes, end?] (an older data layout put the fixes fifth)
    return r.i.map(i => ({ start: words[i[0]][0], severity: i[1], category: i[2], fixes: Array.isArray(i[4]) ? i[4] : i[5] }))
  }
}

// hunspell, in chunks. Flagged words per line (-a), with suggestions only when asked.
function hunspellFlags(lines, withSuggestions) {
  const all = []
  for (let i = 0; i < lines.length; i += 2000) {
    const chunk = lines.slice(i, i + 2000)
    const raw = execFileSync('hunspell', ['-d', DICT, '-a', '-i', 'utf-8'], { input: chunk.map(l => '^' + l).join('\n') + '\n', maxBuffer: 1 << 27 }).toString()
    const blocks = raw.split('\n').slice(1).join('\n').split('\n\n')
    for (let n = 0; n < chunk.length; n++) {
      all.push((blocks[n] || '').split('\n').filter(l => l[0] === '&' || l[0] === '#').map(l => {
        const m = l.match(/^[&#] (\S+)(?: \d+ \d+: (.*))?/)
        return { word: m[1], suggestions: withSuggestions && m[2] ? m[2].split(', ') : [] }
      }))
    }
  }
  return all
}
// Without suggestions: -l lists the misspelt words of each line, much faster.
function hunspellFlaggedLines(lines) {
  let flaggedLines = 0, flaggedWords = 0
  for (let i = 0; i < lines.length; i += 5000) {
    const chunk = lines.slice(i, i + 5000)
    const flagged = execFileSync('hunspell', ['-d', DICT, '-i', 'utf-8', '-L'], { input: chunk.join('\n') + '\n', maxBuffer: 1 << 27 }).toString().split('\n').filter(Boolean)
    const words = execFileSync('hunspell', ['-d', DICT, '-i', 'utf-8', '-l'], { input: chunk.join('\n') + '\n', maxBuffer: 1 << 27 }).toString().split('\n').filter(Boolean)
    flaggedLines += flagged.length; flaggedWords += words.length
  }
  return { flaggedLines, flaggedWords }
}

const corpus = fs.readFileSync('data/corpus.txt', 'utf8').split('\n').filter(Boolean)
const builds = []
for (const spec of specs) builds.push({ ...spec, run: await engine(spec) })
const pct = (n, d) => Math.round(1000 * n / d) / 10

// ---- false alarms ----
const falseAlarms = { sentences: corpus.length, tools: [] }
for (const b of builds) {
  let flagged = 0, remarks = 0, errors = 0
  const bySeverity = { err: 0, warn: 0, info: 0 }
  for (const s of corpus) {
    const r = b.run(s)
    if (r.length) flagged++
    remarks += r.length
    for (const i of r) bySeverity[i.severity]++
    if (r.some(i => i.severity !== 'info')) errors++
  }
  falseAlarms.tools.push({ label: b.label, flaggedSentences: flagged, flaggedPercent: pct(flagged, corpus.length), remarks, bySeverity, sentencesWithErrorOrWarning: errors })
}
const h = hunspellFlaggedLines(corpus)
falseAlarms.tools.push({ label: 'hunspell', flaggedSentences: h.flaggedLines, flaggedPercent: pct(h.flaggedLines, corpus.length), flaggedWords: h.flaggedWords })

// ---- typos ----
let seed = 7
const rand = n => { seed = (seed * 1103515245 + 12345) & 0x7fffffff; return seed % n }
const LETTERS = 'aäbcdeéëfghijklmnopqrstuüvwxyz'
function typo(w) {
  const i = 1 + rand(w.length - 2), kind = rand(4)
  if (kind === 0) return w.slice(0, i) + w.slice(i + 1)
  if (kind === 1) return w.slice(0, i) + w[i + 1] + w[i] + w.slice(i + 2)
  if (kind === 2) return w.slice(0, i) + LETTERS[rand(LETTERS.length)] + w.slice(i + 1)
  return w.slice(0, i) + LETTERS[rand(LETTERS.length)] + w.slice(i)
}
const candidates = []
while (candidates.length < typoCount * 1.4) {
  const line = corpus[rand(corpus.length)]
  const words = [...line.matchAll(/\p{L}{5,}/gu)]
  if (!words.length) continue
  const m = words[rand(words.length)], bad = typo(m[0])
  if (bad === m[0]) continue
  candidates.push({ line: line.slice(0, m.index) + bad + line.slice(m.index + m[0].length), start: m.index, bad, good: m[0], before: line })
}
// Only words every tool accepted before the typo.
const hBefore = hunspellFlags(candidates.map(c => c.before), false)
const kept = candidates.filter((c, n) => !hBefore[n].some(w => w.word === c.good) && builds.every(b => !b.run(c.before).some(i => i.start === c.start))).slice(0, typoCount)
const hAfter = hunspellFlags(kept.map(c => c.line), true)
const typos = { words: kept.length, top: 4, tools: [] }
for (const b of builds) {
  let found = 0, first = 0, top = 0
  for (const c of kept) {
    const i = b.run(c.line).find(i => i.start === c.start)
    if (!i) continue
    found++; if (i.fixes[0] === c.good) first++; if (i.fixes.slice(0, 4).includes(c.good)) top++
  }
  typos.tools.push({ label: b.label, caughtPercent: pct(found, kept.length), firstPercent: pct(first, kept.length), topPercent: pct(top, kept.length) })
}
{
  let found = 0, first = 0, top = 0
  kept.forEach((c, n) => {
    const w = hAfter[n].find(w => w.word === c.bad)
    if (!w) return
    found++; if (w.suggestions[0] === c.good) first++; if (w.suggestions.slice(0, 4).includes(c.good)) top++
  })
  typos.tools.push({ label: 'hunspell', caughtPercent: pct(found, kept.length), firstPercent: pct(first, kept.length), topPercent: pct(top, kept.length) })
}

// ---- grammar ----
const SPELLING = new Set(['spelling', 'compound', 'unlisted', 'proper_noun'])
const last = builds[builds.length - 1]
const wrong = fs.readFileSync('test/new-rules.txt', 'utf8').split('\n').filter(l => l.trim() && !l.startsWith('#')).map(l => l.split(' => ')[0])
const grammarSentences = wrong.filter(s => { const r = last.run(s); return r.length && !r.some(i => SPELLING.has(i.category)) })
const hGrammar = hunspellFlags(grammarSentences, false)
const grammar = {
  sentences: grammarSentences.length,
  tools: builds.map(b => ({ label: b.label, caught: grammarSentences.filter(s => b.run(s).length).length }))
    .concat([{ label: 'hunspell', caught: hGrammar.filter(f => f.length).length }])
}

// ---- certified mistakes (test/mutations.mjs) ----
// LOD sentences with one mistake the rule leaves no doubt about; the LOD sentence is the answer.
// Measured on the last build only (older builds read another data layout). hunspell checks words
// one by one: of these families it can only see the accents.
const { cases: certifiedCases, score: certifiedScore } = await import('./mutations.mjs')
const { load: loadEngine, remarks: engineRemarks, applyFirstFixes } = await import('./engine.mjs')
const lastRun = await loadEngine(last.wasm, last.lod)
const families = {}
for (const c of certifiedCases(last.lod, 'data/corpus.txt')) {
  const f = families[c.family] ??= { cases: 0, caught: 0, fixed: 0 }
  const r = certifiedScore(lastRun, c, engineRemarks, applyFirstFixes)
  f.cases++; if (r >= 1) f.caught++; if (r >= 2) f.fixed++
}
const certified = {
  cases: Object.values(families).reduce((n, f) => n + f.cases, 0),
  families: Object.entries(families).map(([family, f]) => ({ family, cases: f.cases, caughtPercent: pct(f.caught, f.cases), fixedPercent: pct(f.fixed, f.cases) })),
}
certified.fixedPercent = pct(Object.values(families).reduce((n, f) => n + f.fixed, 0), certified.cases)

// ---- speed (last build) ----
const wordCount = corpus.reduce((n, s) => n + (s.match(/\p{L}+/gu) || []).length, 0)
const t0 = performance.now(); for (const s of corpus) last.run(s); const ms = performance.now() - t0
const speed = { words: wordCount, milliseconds: Math.round(ms), msPer1000Words: Math.round(10000 * ms / wordCount) / 10 }

// ---- speller agreement ----
const speller = JSON.parse(execFileSync('node', ['test/spell.mjs', last.wasm, last.lod, 'data/corpus.txt', 'data/spell', '30000', '--json']).toString())

const result = {
  date: new Date().toISOString().slice(0, 10),
  engine: process.env.ENGINE_VERSION || '',
  lod: lodHead[1], spellchecker: lodHead[lodHead.length - 1] || '',
  falseAlarms, typos, grammar, certified, speed, speller
}
const text = JSON.stringify(result, null, 2) + '\n'
if (outFile) fs.writeFileSync(outFile, text); else process.stdout.write(text)
