// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Hand-written cases. Exits 1 when one fails.
//   test/new-rules.txt  wrong sentences, one per line, as "wrong => right": once every remark's
//                       first fix is applied, all at once (the page's "fix all") and one click at
//                       a time with a new analysis after each, the text must be exactly "right",
//                       and "right" must get no remark. A line with no
//                       "=>" only has to get at least one remark (when no fix can be expected).
//   test/no-alarm.txt   correct sentences, none may get a remark
//   test/known.txt      known gaps, reported but not failing: "+ " should get a remark (or reach
//                       its "=> right") and does not yet, "- " should get none and does. A gap
//                       that closes is reported so the line can move to one of the two files above.
// Usage: node test/cases.mjs <letz.wasm> <lod.txt> [--quiet]
import fs from 'fs'
import { load, remarks, applyFirstFixes } from './engine.mjs'
const [W, D, flag] = process.argv.slice(2)
const quiet = flag === '--quiet'
const here = f => new URL(f, import.meta.url)
const run = await load(W, D)
const lines = f => fs.readFileSync(here(f), 'utf8').split('\n').filter(l => l.trim() && !l.startsWith('#'))
const show = (s, r) => { console.log((r.length ? '  ' : '∅ ') + s); for (const i of r) console.log('      [' + i.severity + '] ' + i.category + '/' + i.rule + ' ' + JSON.stringify(i.params) + ' → ' + JSON.stringify(i.fixes) ) }

// What is wrong with a case, or '' when it passes.
function check(line, wantRemark) {
  const [s, right] = line.split(' => ')
  const r = remarks(run(s))
  if (!quiet) show(s, r)
  if (!wantRemark) return r.length ? `remark(s): ${r.map(i => i.rule).join(', ')}` : ''
  if (!r.length) return 'no remark'
  if (right === undefined) return ''
  // All at once (the page's "fix all"), then one click at a time, re-analysing after each.
  const fixed = applyFirstFixes(s, r)
  if (fixed !== right) return `fixes give "${fixed}", want "${right}"`
  let text = s
  for (let n = 0; n < 10; n++) {
    const left = remarks(run(text)).filter(i => i.fixes.length)
    if (!left.length) break
    text = applyFirstFixes(text, left.slice(0, 1))
  }
  if (text !== right) return `fixes clicked one by one give "${text}", want "${right}"`
  const after = remarks(run(right))
  if (after.length) return `"${right}" still gets: ${after.map(i => i.rule + ' ' + JSON.stringify(i.fixes)).join(', ')}`
  return ''
}

const failures = [], closed = []
for (const [file, wantRemark] of [['new-rules.txt', true], ['no-alarm.txt', false]]) {
  if (!quiet) console.log('== test/' + file)
  for (const l of lines(file)) {
    const why = check(l, wantRemark)
    if (why) failures.push(`${file}: ${l.split(' => ')[0]}\n      ${why}`)
  }
}
const wasQuiet = quiet
for (const l of lines('known.txt')) {
  if (!wasQuiet) console.log('== known gap: ' + l)
  if (!check(l.slice(2), l[0] === '+')) closed.push(l)
}
console.log(`\n${failures.length} failing case(s)` + failures.map(f => '\n  ' + f).join(''))
if (closed.length) console.log(`known gaps now closed, move them out of test/known.txt:` + closed.map(f => '\n  ' + f).join(''))
process.exit(failures.length ? 1 : 0)
