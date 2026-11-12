// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Wrong sentences with a certified answer, made from the LOD: take an example sentence of the LOD
// (correct), break one thing in it where the rule leaves no doubt, and the LOD sentence is the
// answer. Nothing comes from the engine, so the cases can judge it.
//
//   n_drop_wrong  a word the LOD writes with its n before a vowel or n d t z h loses it:
//                 "wann et" → "wa et". Kept only when the n-less form never appears before that
//                 next word anywhere in the LOD examples.
//   n_add_wrong   a word the LOD writes without its n before another consonant gets it back:
//                 "de Mann" → "den Mann". Kept only when no word of the LOD keeps its n before
//                 that next word (the LOD writes both "a sengem" and "an sengem": not a case).
//
//   verb_person_wrong  a subject pronoun (ech, du, hien/hie, hatt, mir/mer, dir/der) right before
//                 the present form the LOD tables give for that person gets the form of another
//                 person: "ech spillen" → "ech spills". The new form is no person's form for that
//                 pronoun in any LOD table, nor a past participle anywhere ("du bezuelt" can be a
//                 perfect), and for mir/dir (also datives: "mir ass kal") not a 3rd-person form.
//
// The n-less form of a word comes from the LOD inflections (lod.txt forms marked "n"). Letters
// whose sound may differ from their spelling (c, j, q, x, y: "Job", "Yoga", "Chili") are left out.
//
// Usage as a module: cases(lodTxt, corpusTxt) → [{ family, wrong, right, start, end }]
// As a script: node test/mutations.mjs <engine.wasm> <lod.txt> <corpus.txt> [--list family]
//   prints, per family, the cases where a remark covers the broken word ("caught") and where
//   applying the first fix of those remarks gives back the LOD sentence ("fixed").
import fs from 'node:fs'

const KEEP = new Set([...'aeiouäëéèêöüàâîôûndtzh'])
const DROP = new Set([...'bfgklmprsvw'])
const word = /\p{L}+/gu

// Subject pronouns and the person (0 ech … 5 si) of the verb after them.
const PRONOUNS = { ech: 0, du: 1, hien: 2, hie: 2, hatt: 2, mir: 3, mer: 3, dir: 4, der: 4 }

export function cases(lodFile, corpusFile) {
  const fullToShort = new Map(), shortToFull = new Map()
  // Present forms per person, every LOD table: form → persons it is; table rows to swap from.
  const personsOf = new Map(), participles = new Set(), tables = [], soundsOff = new Set()
  const words = cell => cell.split(' / ').map(alt => alt.split(' ')[0]).filter(Boolean)
  for (const line of fs.readFileSync(lodFile, 'utf8').split('\n')) {
    const f = line.split('\t')
    // Words the LOD says sound otherwise than they are spelt (Job, Gin, Yoga): not certain cases.
    if (f.length === 11 && f[8]) { soundsOff.add(f[1].toLowerCase()); continue }
    if (f.length === 9 && /^\d+$/.test(f[0])) {
      for (const pp of words(f[2])) participles.add(pp)
      const row = f.slice(3, 9).map(words)
      row.forEach((forms, person) => forms.forEach(form => personsOf.set(form, (personsOf.get(form) || 0) | (1 << person))))
      tables.push(row)
      continue
    }
    if (f.length !== 4 || f[3] !== 'n' || !f[2]) continue
    for (const full of f[2].split('|')) {
      if (full === f[0] || full !== f[0] + 'n' && full !== f[0] + 'nn') continue
      fullToShort.set(full, f[0]); shortToFull.set(f[0], full)
    }
  }
  const sentences = fs.readFileSync(corpusFile, 'utf8').split('\n').filter(Boolean)
  // What the LOD itself writes before each next word: with the n, without it.
  const keptBefore = new Set(), droppedBefore = new Set()
  const pairs = s => {
    const out = [], ws = [...s.matchAll(word)]
    for (let i = 0; i + 1 < ws.length; i++) {
      const a = ws[i], b = ws[i + 1]
      if (s.slice(a.index + a[0].length, b.index) !== ' ') continue
      if (a.index > 0 && /[\p{L}'’´-]/u.test(s[a.index - 1])) continue
      // The forms are case-sensitive ("Hunn" the rooster, "hunn" to have): a capital is only
      // dropped at the start of a sentence.
      const first = !/\S/.test(s.slice(0, a.index)) || /[.!?]\s*$/.test(s.slice(0, a.index))
      const key = first && a[0] !== a[0].toLowerCase() && !fullToShort.has(a[0]) && !shortToFull.has(a[0]) ? a[0].toLowerCase() : a[0]
      out.push({ a: a[0], key, start: a.index, end: a.index + a[0].length, next: b[0] })
    }
    return out
  }
  for (const s of sentences) for (const p of pairs(s)) {
    const lower = p.key, next = p.next.toLowerCase()
    if (fullToShort.has(lower)) keptBefore.add(next)
    if (shortToFull.has(lower)) droppedBefore.add(next)
  }
  // Which tables a form belongs to, at a given person.
  const tablesAt = new Map()
  for (const row of tables) row.forEach((forms, person) => forms.forEach(form => {
    const k = person + ' ' + form; if (!tablesAt.has(k)) tablesAt.set(k, []); tablesAt.get(k).push(row)
  }))
  const keepCase = (model, w) => model[0] === model[0].toUpperCase() ? w[0].toUpperCase() + w.slice(1) : w
  const out = []
  for (const right of sentences) for (const p of pairs(right)) {
    const lower = p.key, next = p.next.toLowerCase(), initial = next[0]
    if (next.length < 2 || soundsOff.has(next)) continue
    let family, replacement
    if (fullToShort.has(lower) && KEEP.has(initial) && !droppedBefore.has(next)) {
      family = 'n_drop_wrong'; replacement = fullToShort.get(lower)
    } else if (shortToFull.has(lower) && DROP.has(initial) && !keptBefore.has(next)) {
      family = 'n_add_wrong'; replacement = shortToFull.get(lower)
    } else continue
    replacement = keepCase(p.a, replacement)
    const wrong = right.slice(0, p.start) + replacement + right.slice(p.end)
    out.push({ family, wrong, right, start: p.start, end: p.start + replacement.length })
  }
  // Verb agreement after a subject pronoun.
  for (const right of sentences) {
    const ws = [...right.matchAll(word)]
    for (let i = 0; i + 1 < ws.length; i++) {
      const pr = ws[i], v = ws[i + 1]
      const person = PRONOUNS[pr[0].toLowerCase()]
      if (person === undefined || right.slice(pr.index + pr[0].length, v.index) !== ' ') continue
      if (pr.index > 0 && /[\p{L}'’´-]/u.test(right[pr.index - 1])) continue
      if (v[0] !== v[0].toLowerCase()) continue
      const rows = tablesAt.get(person + ' ' + v[0])
      if (!rows) continue
      const seen = new Set()
      for (const row of rows) row.forEach((forms, other) => {
        const w = forms[0]
        if (other === person || !w || seen.has(w) || w === v[0]) return
        const mask = personsOf.get(w) || 0
        if (mask & (1 << person) || participles.has(w)) return
        if (person >= 3 && person <= 4 && mask & ((1 << 2) | (1 << 5))) return
        seen.add(w)
        out.push({ family: 'verb_person_wrong', wrong: right.slice(0, v.index) + w + right.slice(v.index + v[0].length), right, start: v.index, end: v.index + w.length })
      })
    }
  }
  return out
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const { load, remarks, applyFirstFixes } = await import('./engine.mjs')
  const args = process.argv.slice(2)
  const li = args.indexOf('--list'), listFamily = li < 0 ? null : args.splice(li, 2)[1]
  const [W, lod, corpus] = args
  const run = await load(W, lod)
  const all = cases(lod, corpus), stats = {}
  for (const c of all) {
    const st = stats[c.family] ??= { cases: 0, caught: 0, fixed: 0, misses: [] }
    st.cases++
    const r = score(run, c, remarks, applyFirstFixes)
    if (r >= 1) st.caught++
    if (r >= 2) st.fixed++
    else if (st.misses.length < 400) st.misses.push(c.wrong + (r ? '   (caught, wrong fix)' : ''))
  }
  for (const [family, st] of Object.entries(stats)) {
    console.log(`${family.padEnd(14)} ${String(st.cases).padStart(6)} cases   caught ${(100 * st.caught / st.cases).toFixed(1)}%   fixed ${(100 * st.fixed / st.cases).toFixed(1)}%`)
    if (family === listFamily) for (const m of st.misses) console.log('    ' + m)
  }
}

// 0 missed, 1 a remark covers the broken word, 2 its first fix gives back the LOD sentence.
export function score(run, c, remarks, applyFirstFixes) {
  const on = remarks(run(c.wrong)).filter(r => r.start < c.end && r.end > c.start)
  if (!on.length) return 0
  return applyFirstFixes(c.wrong, on) === c.right ? 2 : 1
}
