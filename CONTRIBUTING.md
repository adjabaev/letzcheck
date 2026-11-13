# Contributing

Thank you for helping. Most changes are either a new language rule or a fix to a rule that
fires where it should not. Both follow the same steps, and both are judged on false alarms first:
people trust a checker only as long as it is right when it speaks.

## Language rules

1. **Silence over a false alarm.** A rule fires only where the LOD leaves no doubt. When a word has
   two genders, two readings (*ee* is an article and the pronoun "one") or a use the rule cannot see
   without parsing the sentence, the rule says nothing.
2. **Every rule comes with sentences.** Wrong sentences it must catch go in `test/new-rules.txt`,
   with the sentence it must become: `Ech hunn zwee Kaz. => Ech hunn zwou Kazen.` Applying the first
   fix of every remark must give exactly that sentence, and that sentence must get no remark: one
   click per mistake, no chain of fixes. Correct sentences that look close and must stay silent go in `test/no-alarm.txt`. A known gap
   you are not fixing goes in `test/known.txt` (`+ ` should be caught, `- ` should stay silent).
3. **No regression.** Run `make check` before every commit: the cases above, then your build
   against the last commit (`test/regress.mjs`), and it fails on any new remark on the corpus, any
   certified mistake caught or fixed before and not now, or any typo caught or ranked worse. The
   certified mistakes (`test/mutations.mjs`) are LOD sentences with one mistake the rule leaves
   no doubt about: n-rule, verb agreement, determiner gender, plural after a number, accents
   (about 126,000); the LOD sentence is the expected correction.
   A regression accepted on purpose goes in `test/accepted.txt`, with its reason. CI runs the same against the previous commit. For details,
   `make corpus BASE=…` compares your build with the previous one
   on 54,996 correct sentences from the LOD. Every new remark it lists is a false alarm to fix. If
   you believe one is right (the corpus has a few real mistakes, mostly spacing), say why in the
   pull request. Remarks the previous build made too, with other parameters or a milder severity,
   are listed as "reworded": check that they are right. If your change alters the data format, compare
   with `BASE=old.wasm@old-lod.txt`.
4. **The LOD is the source.** Gender, plurals, conjugations and spellings come from the data, not from
   lists in the code. The spellchecker.lu dictionary only decides whether a word is spelt right and
   which corrections to offer; grammar rules never rely on it. A hand-written list (units, interjections, conjunctions…) needs a comment
   saying why the LOD cannot provide it.
5. **No sentences in the engine.** A remark is a category, a rule key and named parameters
   (`Issue::new(…, "n_drop", params(&[("next", …), ("fixed", …)]), fixes)`); writing the message is
   the job of the product that uses the engine. A new rule gets a new key, documented in the
   Remarks table of the README with its parameters. Keep rule keys and parameter names stable:
   products depend on them.
6. **Fixes only when safe.** Give a fix when it is the only reasonable correction; otherwise leave
   the list empty, and give the product the parameters it needs to explain why.

The workflow:

```sh
make wasm && cp dist/letz.wasm /tmp/base.wasm   # the engine before your change
# edit src/lib.rs and the test files
make test                                      # fails if a case breaks
make corpus BASE=/tmp/base.wasm                # new remarks = false alarms, plus lost remarks
```

A rule is a `check_…` function that reads the `Analysis` (the dictionary, the words, where
sentences start) and pushes `Issue`s; `analyze_text` calls the checks in order. A rule that looks at
one word and its neighbours goes in the second loop there. The order matters when two rules touch
the same word: a later rule may drop an earlier remark (`drop_n_rule`) or adjust its fixes.

## Code

The code follows the [Google Rust style guide](https://google.github.io/styleguide/rustguide.html),
which itself builds on the official Rust style guide and API guidelines. CI enforces the parts a tool
can check:

```sh
cargo fmt --check                                                  # rustfmt.toml: 100 columns
cargo clippy --release --target wasm32-unknown-unknown -- -D warnings
```

Points of the guide that matter most here:

- **`unsafe`** stays at the edge of the module (the exports and the module state). Every
  `unsafe` block has a `// SAFETY:` comment, and every `unsafe fn` a `# Safety` section. The state
  invariants are written once above the statics in `src/lib.rs`; comments refer to them by number.
- **Panics** are bugs. Prefer `if let` / `let … else` to checking then unwrapping, and `expect("why
  this holds")` to `unwrap()`, which the engine does not use. The crate is built with
  `panic = "abort"`.
- **Names** spell words out: `word_index`, `dictionary`, `next_initial`, not `wi`, `db`, `ni`. Loop
  counters (`i`, `j`), a `char` called `c` and the linguistic abbreviations of the LOD itself
  (`SUBST`, `VRB`, A1/A2) are the exceptions.
- **Comments** say why, not what. Document what a public export returns and when it fails.
- **No dependencies.** The engine must stay small and build with the standard library alone.

Where this project departs from the guide:

- The public API is the C ABI listed in the README, not Rust types, so the guide's API naming rules
  (getters, constructors, traits) apply only to internal code.
- Tests are the scripts in `test/` run on the compiled wasm, not `#[test]` functions: what matters is
  the module a page loads.
- The engine is one file, `src/lib.rs`. Splitting it into modules is welcome as a change of its own,
  with no behaviour change, and not mixed with a rule.

## Pull requests

- One change per pull request; a rule and a refactor go in separate ones.
- Formatting-only or refactoring changes must leave the output unchanged: `make same BASE=…`
  compares everything the two builds return on the corpus and on 40,000 random texts.
- CI runs formatting, Clippy, the cases, the speller against hunspell (`make spell`) and the
  corpus against the base branch.

By contributing you agree that your contribution is licensed under the
[Mozilla Public License 2.0](LICENSE). The LOD data and the spellchecker.lu dictionary stay under
their own CC0 licence and are never committed to this repository.
