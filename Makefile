# Lëtzcheck engine. Needs: Rust with the wasm32-unknown-unknown target, Python 3, Node >= 20.
LOD ?= --version 260727
SPELL ?=
WASM := target/wasm32-unknown-unknown/release/letz.wasm

.PHONY: all data wasm test check spell bench corpus same clean
all: data wasm

data: data/lod.txt data/corpus.txt

data/lod/VERSION:
	python3 tools/fetch_lod.py $(LOD) --out data/lod

data/spell/VERSION:
	python3 tools/fetch_spell.py $(SPELL) --out data/spell

data/lod.txt: data/lod/VERSION data/spell/VERSION tools/build_lod.py tools/convert.mjs
	python3 tools/build_lod.py data/lod data/lod.json
	node tools/convert.mjs data/lod.json data/lod.txt data/spell

data/corpus.txt: data/lod/VERSION tools/lod_examples.py
	python3 tools/lod_examples.py data/lod/art/new_lod-art.xml data/corpus.txt

wasm:
	cargo build --release --target wasm32-unknown-unknown
	mkdir -p dist && cp $(WASM) dist/letz.wasm
	@if command -v wasm-opt >/dev/null 2>&1; then \
	  wasm-opt -O3 --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext --enable-mutable-globals dist/letz.wasm -o dist/letz.wasm; \
	fi
	@ls -l dist/letz.wasm

# Hand-written cases (test/new-rules.txt, test/no-alarm.txt); fails on a broken case.
test: wasm data/lod.txt
	node test/cases.mjs dist/letz.wasm data/lod.txt

# Before every commit: the hand-written cases, then the new build against the one it replaces
# (HEAD, or REF=<git revision>), item by item: no new false alarm on the corpus, no typo handled
# worse. The base is built once per revision in target/base/.
REF ?= HEAD
check: wasm data
	node test/cases.mjs dist/letz.wasm data/lod.txt --quiet
	node test/regress.mjs "$$(sh tools/base_build.sh $(REF))" dist/letz.wasm data/lod.txt data/corpus.txt

# src/spell.rs against the real hunspell program (apt install hunspell): same verdict on every word.
spell: wasm data
	node test/spell.mjs dist/letz.wasm data/lod.txt data/corpus.txt data/spell

# Quality bench against the spellchecker.lu dictionary run by hunspell: false alarms on the corpus,
# typos, grammar cases, speed, speller agreement. BASE=old.wasm[@old-lod.txt] adds an older build.
bench: wasm data
	node test/bench.mjs --out dist/bench.json $(if $(filter-out -,$(BASE)),base=$(BASE)) current=dist/letz.wasm
	@cat dist/bench.json

# Remarks on the corpus of correct LOD sentences, i.e. false alarms. Alone it lists them all;
# with BASE=path/to/old.wasm it lists only what changed against that build (new remarks, lost ones).
# An older build that reads another data format: BASE=old.wasm@old-lod.txt (also for make same).
BASE ?= -
corpus: wasm data
	node test/corpus.mjs $(BASE) dist/letz.wasm data/lod.txt data/corpus.txt

# For refactoring: the full output must not change at all. BASE=path/to/old.wasm make same
same: wasm data
	@test "$(BASE)" != "-" || { echo "set BASE to the wasm to compare with"; exit 2; }
	node test/same-output.mjs $(BASE) dist/letz.wasm data/lod.txt data/corpus.txt

clean:
	rm -rf target dist data
