# Lëtzcheck engine

A rule-based checker for Luxembourgish. It reads a text and points out
misspellings, the n-rule (Eifeler Regel), article gender, plurals, verb agreement with the subject,
capitalisation and a few typographic habits, each as a structured remark (rule and parameters)
with a fix when one is safe.

It is a small Rust crate with no dependencies, compiled to WebAssembly (about 250 KB), and runs
entirely on the user's device: no text leaves the page. It powers [letz.arby.be](https://letz.arby.be).

What it knows about words comes from two sources of the Zenter fir d'Lëtzebuerger Sprooch, both
open data under CC0:

- the **Lëtzebuerger Online Dictionnaire (LOD)**, on [data.public.lu](https://data.public.lu/):
  forms, gender, plurals, conjugations, pronunciation, translations;
- the **spellchecker.lu** Hunspell dictionary, from its
  [package repository](https://downloads.spellchecker.lu/packages/Ubuntu/): about 97,000 words with
  their inflections, which accepts many correct words the LOD has no entry for and improves the
  corrections offered for typos.

This repository ships neither: `make data` downloads them and builds the engine's index.

```
$ node examples/check.mjs "Ech hunn ee Fro. Hien spillen zu dem Bësch."
warn ee       gender/gender_determiner {"noun":"Fro","gender":"F","fixed":"eng Fro","current":"ee Fro"}  → eng
warn Hien     n_rule/n_drop {"next":"spillen","initial":"s","sound":"","fixed":"Hie"}  → Hie
err  spillen  verb/verb_form {"pronoun":"hien","verb":"spillen","lemma":"spillen","expected":"hien spillt"}  → spillt
warn zu       contraction/contraction {"phrase":"zu dem","contracted":"zum"}  → zum
```

The engine writes no sentences. Each remark is a rule key with named parameters and the fixes;
the product that uses the engine writes its own messages, in its own languages and voice (see
[Remarks](#remarks) for every rule).

## Build

Needs Rust with the `wasm32-unknown-unknown` target, Python 3 and Node 20 or later.

```sh
rustup target add wasm32-unknown-unknown   # once
make            # downloads the LOD (about 13 MB of zips) and the spellchecker.lu package (0.6 MB),
                # builds data/lod.txt and dist/letz.wasm
make test       # the hand-written cases in test/; fails if one breaks
make corpus     # every remark on 54,996 correct sentences from the LOD (all false alarms)
make spell      # src/spell.rs against the real hunspell program, word by word (needs hunspell)
```

`wasm-opt` (binaryen) is used when it is installed; without it the plain cargo output is kept.

### Data version

`make data` fetches the LOD release of 27 July 2026 by default, checked against pinned SHA-256
sums, so everyone measures the rules on the same data. Other releases:

```sh
make clean && make LOD=--latest           # newest release published for all three datasets
make clean && make LOD="--version 260518"
```

The spellchecker.lu package is pinned the same way (version 2.1-2026-06-01, SHA-256 in
`tools/fetch_spell.py`); `make SPELL=--latest` takes the version the repository serves now. It is
unpacked without being installed.

Both fetch scripts stop if the data is ever published under a licence other than CC0.

## What it checks

| Category | Example | Source |
|---|---|---|
| Spelling | unknown words, known misspellings, missing accents, close forms; numbers written in one word (*zweeasechzegdausendfënnefhonnert*) are read, not flagged | LOD forms and spellings, spellchecker.lu dictionary |
| n-rule | *Hien spillt* → *Hie spillt*; *e Haus* → *en Haus*; before Y it follows the sound: *den Yoga* → *de Yoga*, *de Yttrium* → *den Yttrium* | LOD n-rule variants and pronunciation |
| Gender | *ee Fro* → *eng Fro*; *eng Bréif* → *e Bréif*; *De Fra* → *D'Fra* | LOD gender |
| Plural | *dräi Kand* → *dräi Kanner* | LOD plurals |
| Verb | *hien schaffen* → *hie schafft*; *ze spillt* → *ze spillen* | LOD conjugation tables |
| Contraction | *vun dem Bësch* → *vum Bësch* | built-in |
| Capitals | nouns, sentence starts | LOD + built-in |
| Typography | *Hond Hond*; a space before `, . ; : ? !` | built-in |

Each remark has a severity: `err` (wrong), `warn` (very likely wrong) or `info` (worth a look).

The engine prefers silence to a false alarm. A rule only fires where the LOD is unambiguous: *een
Iddi* is not flagged because the LOD has *Iddi* both as a feminine noun (idea) and a masculine one
(liking); *ee* after a conjunction is read as the pronoun "one", not as an article. It does not parse
sentences, so word order, case after prepositions and agreement across a clause are out of reach.

## Quality bench

```sh
sudo apt install hunspell            # the reference: the spellchecker.lu dictionary run by hunspell
make bench                           # writes dist/bench.json
make bench BASE=old.wasm@old-lod.txt # adds an older build to compare with
```

`test/bench.mjs` puts every build and the spellchecker.lu dictionary (run by the real `hunspell`)
through the same tests:

- **false alarms**: the share of the 54,996 correct LOD sentences that get a remark (they come from
  the LOD, the engine's own source, which favours it here);
- **typos**: 2,000 corpus words with one typo each (a letter missing, swapped, replaced or added;
  fixed seed): is it caught, is the right word the first fix, is it among the first four;
- **grammar**: the wrong sentences of `test/new-rules.txt` where every word exists;
- **speed** of the analysis, and the **speller agreement** of `make spell`.

[letz.arby.be](https://letz.arby.be/#quality) draws its quality section from that JSON.

## Contributing

New rules and fixes are welcome: see [CONTRIBUTING.md](CONTRIBUTING.md). In short, every rule comes
with test sentences, and `make corpus BASE=…` must show no new remark on the corpus of correct
sentences. The code follows the Google Rust style guide; CI checks formatting, Clippy, the cases and
the corpus.

The current build makes 2,497 remarks on the 54,996 corpus sentences (3,021 before the
spellchecker.lu dictionary). Many come from the way the
sentences are rebuilt (the LOD stores examples as word lists, so abbreviations such as *k.o.* or
fragments such as *(d)e* come out odd); the others are known false alarms worth reducing.

## Using the module

The module exports a plain C ABI, with no wasm-bindgen. `examples/check.mjs` shows the whole
cycle in about thirty lines.

```
alloc(len) -> ptr, dealloc(ptr, len)
init(ptr, len) -> 0 | error code     lod.txt bytes; the module takes ownership
analyze(ptr, len) -> out             UTF-8 text in, JSON out
describe(wi), gloss(wi, lang), nfull(wi), clitic(wi) -> out; level(wi) -> i32
                                     about word wi of the latest analyze(); clitic() gives the
                                     clitic itself ("s", "d'", "z'", "'t", "'n", "'s") or ""
                                     gloss(wi, lang): the LOD translation, 0 French, 1 English,
                                     2 German, 3 Portuguese
suggest(ptr, len, max) -> out, version() -> out, entries() -> i32, fold(ptr, len) -> out
spell(ptr, len) -> i32               1 if the spellchecker.lu dictionary accepts the word
number(wi) -> out                    the value of word wi when it is a number in words ("1815"), or ""
```

`out` is a pointer to a u32 little-endian length followed by UTF-8, valid until the next call.
`analyze` returns:

```json
{"k":"wswswswp","e":[3,4,8,9,11,12,15,16],
 "w":[[0,2,0,"ech"],[0,2,0],[0,2,2],[0,2,0]],
 "i":[[2,"warn","gender","gender_determiner",{"noun":"Fro","gender":"F","fixed":"eng Fro","current":"ee Fro"},["eng"]]],
 "s":[4,4,2,1,0,1,0]}
```

- `k`: one letter per token (`w` word, `s` spaces and line breaks, `p` punctuation, `n` number).
- `e`: the end offset of each token, in UTF-16 code units, so it indexes a JavaScript string directly.
- `w`: per word, `[status, flags, severity, normalised form?]`. Status: 0 in the LOD, 1 in the LOD
  with another case, 2 known misspelling, 3 compound, 4 seen in LOD examples, 5 capitalised and
  unknown, 6 unknown, 7 not in the LOD but spelt right according to spellchecker.lu, 8 a number
  written in words (`aachtdausendzweehonnertzweeasechzeg`), 9 a number written in words to fix
  (accents, or millions written together).
- `i`: the remarks, as `[word index, severity, category, rule, params, fixes, end?]`. The category
  and the rule are stable keys, the params an object of strings (see below). The optional `end` is
  the UTF-16 offset where the fix stops when it spans more than the word (`zu dem` → `zum`). An empty
  fix means "delete the word".
- `s`: words, words known to the LOD, A1 words, A2 words, then the `err`, `warn` and `info` counts.

### Remarks

`source` and `close_source` say which dictionary spells the word offered: `lod`, or `spellchecker`
when only the spellchecker.lu dictionary has it; `close` is the first fix (absent without fixes).

Categories: `spelling`, `compound`, `unlisted`, `proper_noun`, `capital`, `n_rule`, `plural`,
`gender`, `verb`, `contraction`, `typography`. Rules and their parameters (all strings; an empty
string means "none"):

| Category | Rule | Parameters | When |
|---|---|---|---|
| spelling | `known_misspelling` | word | a misspelling the LOD lists |
| spelling | `missing_accent` | word, accented, source | the same letters with more accents are a known word |
| spelling | `accent_variant` | word, accented, source | the same letters with other accents are a known word |
| spelling | `unknown_word` | word, close?, close_source? | unknown; the fixes may be empty |
| spelling | `unknown_capitalised` | word, close?, close_source? | unknown, capitalised mid-sentence, with close words |
| compound | `compound` | word, first, first_fr/en/de/pt, second, second_fr/en/de/pt | looks like first + second (with their LOD translations) |
| unlisted | `unlisted_attested` | word | no entry, but used in LOD examples |
| spelling | `number_spelling` | word, expected, value | a number in words with wrong accents (*uechtzenghonnert…* → *uechtzénghonnert…*, 1815) |
| spelling | `number_split` | word, expected, value | millions written together: *Millioun(en)* stands apart |
| proper_noun | `proper_noun` | word, close?, close_source? | unknown, capitalised: maybe a name |
| capital | `capital_unexpected` | word, lower, gloss_fr/en/de/pt | the LOD has it in lower case only |
| capital | `capital_missing` | noun, gloss_fr/en/de/pt | a noun written in lower case |
| capital | `capital_start` | fixed | a sentence starting in lower case |
| n_rule | `n_keep` | next, before (`n` `d` `t` `z` `h` or `vowel`), full | the n must stay |
| n_rule | `n_end` | full | the n must stay at the end of a sentence |
| n_rule | `n_drop` | next, initial, sound (`j` for a Y said [j], else empty), fixed | the n must drop |
| plural | `plural_determiner` | noun, lemma (when it differs), fixed, current | singular determiner before a plural |
| plural | `number_plural` | number, plural | a number before a singular noun |
| plural | `number_gender_plural` | noun, gender, plural, fixed, current | zwee / zwou of the wrong gender before a singular noun: one fix for both words ("zwee Kaz" → "zwou Kazen") |
| gender | `gender_determiner` | noun, gender (`M` `F` `N`), fixed, current | determiner of the wrong gender |
| verb | `verb_form` | pronoun, verb, lemma, expected | wrong person, with the right form |
| verb | `verb_mismatch` | pronoun, verb | wrong person, no form to offer |
| verb | `ze_infinitive` | verb | *ze* + something else than the infinitive |
| contraction | `contraction` | phrase, contracted | *vun dem* → *vum* |
| typography | `repeated_word` | word | the same word twice |
| typography | `space_before` | mark | a space before `, . ; : ? !` |

## Layout

```
src/lib.rs            the engine
src/spell.rs          reader for the spellchecker.lu Hunspell dictionary (affixes, no compounding)
src/nfd.txt           Unicode decompositions, generated by tools/gen-nfd.mjs
tools/fetch_lod.py    downloads the LOD datasets from data.public.lu
tools/fetch_spell.py  downloads the spellchecker.lu Hunspell package and unpacks the dictionary
tools/build_lod.py    LOD XML -> data/lod.json (forms, gender, plurals, conjugations, levels, Y sounds)
tools/convert.mjs     lod.json + the Hunspell dictionary -> lod.txt, the line-based file the engine loads
tools/lod_examples.py LOD example sentences -> data/corpus.txt
test/                 hand-written cases (new-rules, no-alarm, known gaps), the corpus comparison,
                      the full-output comparison for refactoring (make same) and the speller check
examples/check.mjs    minimal Node usage
```

## Licence

The code is under the [Mozilla Public License 2.0](LICENSE): you can use the engine in any project,
open or closed, but changes to its files must be published under the same licence.

The LOD data and the spellchecker.lu dictionary are © Zenter fir d'Lëtzebuerger Sprooch, released
under CC0 1.0. They are downloaded at build time and are not part of this repository. Please keep
crediting them wherever the engine is used.
