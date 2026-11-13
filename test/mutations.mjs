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
//   gender_wrong  a determiner right before a singular noun of one gender in every LOD entry gets
//                 the form of another gender: "de Mann" → "d'Mann", "eng Fro" → "e Fro", "dem Bësch"
//                 → "der Bësch", "meng Mamm" → "mäi Mamm". Only when the LOD never writes that
//                 determiner before that noun ("der Däiwel" is in the LOD: not a case).
//   number_plural_wrong  a number word before a plural the LOD lists for a noun gets the singular:
//                 "zwee Hënn" → "zwee Hond". Only nouns whose singular the LOD never writes after a
//                 number ("dräi Mol", "zwee Liter": not cases).
//
//   accent_wrong  one accent of a word of the sentence is dropped ("Schnéi" → "Schnei"), only when
//                 the bare form is no LOD form in any case and the spellchecker.lu dictionary (run
//                 by hunspell) rejects it too ("Bänk" → "Bank" is a word: not a case). One per
//                 sentence, in one sentence out of seven.
//                 Left out when hunspell is not installed.
//
// The n-less form of a word comes from the LOD inflections (lod.txt forms marked "n"). Letters
// whose sound may differ from their spelling (c, j, q, x, y: "Job", "Yoga", "Chili") are left out.
//
// Usage as a module: cases(lodTxt, corpusTxt) → [{ family, wrong, right, start, end }]
// As a script: node test/mutations.mjs <engine.wasm> <lod.txt> <corpus.txt> [--list family]
//   prints, per family, the cases where a remark covers the broken word ("caught") and where
//   applying the first fix of those remarks gives back the LOD sentence ("fixed").
import fs from 'node:fs'
import { execFileSync } from 'node:child_process'
import path from 'node:path'

const KEEP = new Set([...'aeiouäëéèêöüàâîôûndtzh'])
const DROP = new Set([...'bfgklmprsvw'])
const word = /\p{L}+/gu

// Subject pronouns and the person (0 ech … 5 si) of the verb after them.
const PRONOUNS = { ech: 0, du: 1, hien: 2, hie: 2, hatt: 2, mir: 3, mer: 3, dir: 4, der: 4 }

export function cases(lodFile, corpusFile) {
  const fullToShort = new Map(), shortToFull = new Map()
  // Present forms per person, every LOD table: form → persons it is; table rows to swap from.
  const personsOf = new Map(), participles = new Set(), tables = [], soundsOff = new Set(), nouns = []
  const words = cell => cell.split(' / ').map(alt => alt.split(' ')[0]).filter(Boolean)
  for (const line of fs.readFileSync(lodFile, 'utf8').split('\n')) {
    const f = line.split('\t')
    // Words the LOD says sound otherwise than they are spelt (Job, Gin, Yoga): not certain cases.
    if (f.length === 11) {
      if (f[8]) soundsOff.add(f[1].toLowerCase())
      if (f[2] === 'SUBST') nouns.push({ lemma: f[1], gender: f[3], plurals: f[7] ? f[7].split(', ') : [] })
      continue
    }
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
  // Gender of the determiner. A noun is certain when every LOD entry with that spelling is a noun
  // of the same single gender, and the spelling is no plural of any noun.
  // ("Mann" is also a plural of "Mann": "d'Mann" can be right)
  const genderOf = new Map(), pluralForms = new Map(), selfPlural = new Set()
  for (const n of nouns) {
    genderOf.set(n.lemma, genderOf.has(n.lemma) && genderOf.get(n.lemma) !== n.gender ? '?' : n.gender)
    for (const p of n.plurals) if (p === n.lemma) selfPlural.add(p)
    for (const p of n.plurals) if (p && p !== n.lemma && !p.includes(' ')) {
      if (!pluralForms.has(p)) pluralForms.set(p, new Set())
      pluralForms.get(p).add(n.lemma)
    }
  }
  const nounGender = w => { const g = genderOf.get(w); return g && /^[MFN]$/.test(g) && !pluralForms.has(w) && !selfPlural.has(w) ? g : '' }
  // determiner → [gender it fits, the form for the other genders]; "N" uses the masculine forms
  // except for the definite article.
  const SWAP = {
    den: ['M', { F: "d'" }], de: ['M', { F: "d'" }], "d'": ['FN', { M: 'den|de' }],
    en: ['MN', { F: 'eng' }], e: ['MN', { F: 'eng' }], eng: ['F', { M: 'en|e', N: 'en|e' }],
    dem: ['MN', { F: 'der' }], der: ['F', { M: 'dem', N: 'dem' }],
    engem: ['MN', { F: 'enger' }], enger: ['F', { M: 'engem', N: 'engem' }],
    'mäin': ['MN', { F: 'meng' }], 'mäi': ['MN', { F: 'meng' }], meng: ['F', { M: 'mäin|mäi', N: 'mäin|mäi' }],
    'däin': ['MN', { F: 'deng' }], 'däi': ['MN', { F: 'deng' }], deng: ['F', { M: 'däin|däi', N: 'däin|däi' }],
    'säin': ['MN', { F: 'seng' }], 'säi': ['MN', { F: 'seng' }], seng: ['F', { M: 'säin|säi', N: 'säin|säi' }],
    keen: ['MN', { F: 'keng' }], kee: ['MN', { F: 'keng' }], keng: ['F', { M: 'keen|kee', N: 'keen|kee' }],
  }
  // What the LOD writes: determiner + noun pairs, number + noun pairs.
  const detPairs = new Set(), numberPairs = new Set()
  const NUMBERS = new Set(['zwee', 'zwou', 'dräi', 'véier', 'fënnef', 'sechs', 'siwen', 'siwe', 'aacht', 'néng', 'zéng', 'eelef', 'zwielef', 'honnert', 'dausend'])
  const detRe = /(?<![\p{L}'’´-])(d['’]|\p{L}+ )(\p{Lu}\p{L}*)/gu
  for (const s of sentences) for (const m of s.matchAll(detRe)) {
    const det = m[1].trim().toLowerCase().replace('’', "'")
    detPairs.add(det + ' ' + m[2])
    if (NUMBERS.has(det)) numberPairs.add(det + ' ' + m[2])
  }
  const pick = (variants, noun) => {
    const [withN, withoutN] = variants.split('|')
    if (!withoutN) return withN
    const initial = noun[0].toLowerCase()
    return KEEP.has(initial) ? withN : DROP.has(initial) ? withoutN : ''
  }
  for (const right of sentences) for (const m of right.matchAll(detRe)) {
    const raw = m[1], noun = m[2], det = raw.trim().toLowerCase().replace('’', "'")
    const at = m.index, before = right.slice(0, at)
    // number + plural → number + singular
    if (NUMBERS.has(det) && pluralForms.has(noun) && !genderOf.has(noun)) {
      const singulars = [...pluralForms.get(noun)]
      const singular = singulars[0]
      if (singulars.length === 1 && ![...NUMBERS].some(n => numberPairs.has(n + ' ' + singular))) {
        const start = at + raw.length
        out.push({ family: 'number_plural_wrong', wrong: right.slice(0, start) + singular + right.slice(start + noun.length), right, start, end: start + singular.length })
      }
      continue
    }
    const swap = SWAP[det], gender = nounGender(noun)
    if (!swap || !gender || !swap[0].includes(gender) || soundsOff.has(noun.toLowerCase())) continue
    if (/\bs\s*$/.test(before)) continue          // "wann s de Zäit hues": "de" is "du"
    for (const [other, variants] of Object.entries(swap[1])) {
      if (other === gender) continue
      const form = pick(variants, noun)
      if (!form || detPairs.has(form + ' ' + noun)) continue
      // keep the case of the determiner as written
      const shown = raw[0] === raw[0].toUpperCase() ? form[0].toUpperCase() + form.slice(1) : form
      const glue = shown.endsWith("'") ? '' : ' '
      const wrong = before + shown + glue + right.slice(at + raw.length)
      out.push({ family: 'gender_wrong', wrong, right, start: at, end: at + shown.length + glue.length + noun.length })
      break
    }
  }
  // Accents: the bare form must be no word at all.
  const known = new Set()
  for (const line of fs.readFileSync(lodFile, 'utf8').split('\n')) {
    const f = line.split('\t')
    if (f.length === 4 && f[0]) known.add(f[0].toLowerCase())
  }
  const BARE = { 'é': 'e', 'ë': 'e', 'è': 'e', 'ê': 'e', 'ä': 'a', 'à': 'a', 'â': 'a', 'ö': 'o', 'ô': 'o', 'ü': 'u', 'û': 'u', 'î': 'i' }
  const accentCandidates = []
  // A fixed sample (one sentence in 7): misspelt words make the engine search for suggestions,
  // which is slow over tens of thousands of cases.
  for (const [n, right] of sentences.entries()) {
    if (n % 7) continue
    for (const m of right.matchAll(word)) {
      if (m.index > 0 && /['’´-]/.test(right[m.index - 1])) continue
      const w = m[0], at = [...w].findIndex(ch => BARE[ch.toLowerCase()])
      if (at < 0 || w.length < 3) continue
      const chars = [...w], ch = chars[at]
      chars[at] = ch === ch.toLowerCase() ? BARE[ch] : BARE[ch.toLowerCase()].toUpperCase()
      const bare = chars.join('')
      if (known.has(bare.toLowerCase())) continue
      accentCandidates.push({ right, start: m.index, word: w, bare })
      break
    }
  }
  const dict = path.join(path.dirname(lodFile), 'spell', 'lb_LU')
  let rejected = null
  try {
    const words = [...new Set(accentCandidates.map(c => c.bare))]
    rejected = new Set(execFileSync('hunspell', ['-l', '-d', dict], { input: words.join('\n'), maxBuffer: 64 << 20 }).toString().split('\n'))
  } catch (e) {
    process.stderr.write('accent_wrong left out: hunspell not available\n')
  }
  if (rejected) for (const c of accentCandidates) {
    if (!rejected.has(c.bare)) continue
    out.push({ family: 'accent_wrong', wrong: c.right.slice(0, c.start) + c.bare + c.right.slice(c.start + c.word.length), right: c.right, start: c.start, end: c.start + c.bare.length })
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
