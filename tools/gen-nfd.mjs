// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Generates src/nfd.txt: canonical decompositions (NFD) for code points below U+3100,
// taken from the JS runtime's own String.prototype.normalize so Rust folds exactly like a JS string.
// Line format: <char><its NFD>\n  (Hangul is handled algorithmically in Rust.)
import { writeFileSync } from 'node:fs';
const out = [];
for (let cp = 0x80; cp < 0x3100; cp++) {
  if (cp >= 0xd800 && cp <= 0xdfff) continue;
  const s = String.fromCodePoint(cp), d = s.normalize('NFD');
  if (d !== s) out.push(s + d);
}
const dest = new URL('../src/nfd.txt', import.meta.url);
writeFileSync(dest, out.join('\n') + '\n');
console.log('nfd.txt:', out.length, 'entries (Unicode ' + process.versions.unicode + ')');
