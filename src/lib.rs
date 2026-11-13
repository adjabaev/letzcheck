// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Lëtzcheck: a rule-based Luxembourgish checker, on top of the LOD open data and the spellchecker.lu
//! Hunspell dictionary (both CC0, Zenter fir d'Lëtzebuerger Sprooch).
//! No dependencies, plain C ABI, built for wasm32-unknown-unknown.
//!
//! The engine started as a port of an earlier JavaScript implementation, called "the reference"
//! in the comments below, and keeps its behaviour, quirks included. JS strings are UTF-16: every
//! place where the reference depends on code units (offsets, lengths, edit distance, the `(.)\1+`
//! collapse, string ordering) works on UTF-16 units here too, so offsets map directly onto a JS string.
//!
//! ABI (wasm32, single-threaded):
//!   alloc(len) -> ptr, dealloc(ptr, len)
//!   init(ptr, len) -> 0 | error code     takes ownership of the buffer (lod.txt, UTF-8)
//!   analyze(ptr, len) -> out              text as UTF-8; the caller keeps ownership
//!   describe(wi) / gloss(wi, lang) / nfull(wi) / clitic(wi) -> out, level(wi) -> i32
//! Remarks are structured (category, rule key, named parameters, fixes): the engine writes no
//! sentences, the product that uses it does.
//!       about word `wi` of the most recent analyze() call
//!   suggest(ptr, len, max) -> out, fold(ptr, len) -> out, version() -> out
//!   spell(ptr, len) -> i32              1 if the spellchecker.lu dictionary accepts the word
//!   number(wi) -> out                   value of word wi if it is a number in words, as plain text
//! `out` points to a u32 little-endian byte length followed by UTF-8 (JSON unless noted),
//! valid until the next call.
#![deny(unsafe_op_in_unsafe_fn)]

mod spell;

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

// ---------- small infrastructure ----------
struct FnvHasher(u64);
impl Default for FnvHasher {
    fn default() -> Self {
        FnvHasher(0xcbf2_9ce4_8422_2325)
    }
}
impl Hasher for FnvHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn finish(&self) -> u64 {
        self.0
    }
}
type Map<K, V> = HashMap<K, V, BuildHasherDefault<FnvHasher>>;
type Set<K> = HashSet<K, BuildHasherDefault<FnvHasher>>;
type StaticStr = &'static str;

/// The languages of the LOD translations kept, in the order of Entry::translations and of the
/// `lang` argument of gloss().
const LANGUAGES: [&str; 4] = ["fr", "en", "de", "pt"];

struct Entry {
    id: StaticStr,
    lemma: StaticStr,
    part_of_speech: StaticStr,
    gender: StaticStr,
    level: u8,
    /// The LOD translations, "; "-separated, in the order of LANGUAGES.
    translations: [StaticStr; 4],
    plural: StaticStr,
    /// How the word starts to the ear where its spelling misleads (c j q x y, g as [dʒ], a vowel
    /// read [j], initialisms): b'v' when the n-rule keeps the n before it (Job, Yttrium), b'j'
    /// when it drops (Yoga, User), 0 when the letter tells. From the LOD pronunciation.
    y_onset: u8,
}

/// One line of the form index: entry ids ("!" prefix = known misspelling), full n-forms, n flag.
#[derive(Clone, Copy)]
struct FormRecord {
    entry_ids: StaticStr,
    full_forms: StaticStr,
    only_n_variant: bool,
}

struct Determiner {
    genders: StaticStr,
    by_gender: [Option<String>; 3],
    plural: Option<String>,
}

struct Dictionary {
    version: StaticStr,
    entries: Vec<Entry>,
    forms: Map<StaticStr, FormRecord>,
    n_dropped: Map<StaticStr, StaticStr>,
    attested: Set<StaticStr>,
    person_masks: Map<StaticStr, (u32, bool)>,
    conjugations: Map<u32, [StaticStr; 8]>,
    nfd: Map<char, StaticStr>,
    determiners: HashMap<String, Determiner>,
    /// The spellchecker.lu Hunspell dictionary: words the LOD lacks but that are spelt right.
    speller: spell::Speller,
    /// Corrections already computed by the speller, by word: the page analyses the whole text
    /// again on every keystroke, and looking for corrections is the slow part. Single-threaded
    /// (rule 1 of the module state), so a RefCell is enough.
    correction_cache: std::cell::RefCell<Map<String, Vec<String>>>,
}

// ---------- module state ----------
// The engine keeps its state in the five `static mut` below. That is sound
// because of how the module runs, which every `SAFETY:` comment in this file relies on:
//   1. Single thread. The crate targets wasm32-unknown-unknown without the atomics feature, so there
//      are no threads, and the exported functions never call one another or back into JS.
//   2. No reference outlives the export call that took it. Each export borrows the state, builds its
//      answer and returns, so a `&'static` handed out by `dictionary()`, `last_word()` or `fold_index()` is dead
//      before the next call can replace what it points to.
//   3. The pointer returned to JS by `output()` is documented as valid only until the next call.
// Statics are always reached through `&raw const` / `&raw mut`, never `&STATIC`, so no reference to
// a `static mut` is ever created implicitly.
static mut DICTIONARY: Option<Dictionary> = None;
static mut FOLD_INDEX: Option<Map<Vec<u16>, Vec<StaticStr>>> = None;
static mut LAST_WORDS: Vec<Word> = Vec::new();
static mut OUTPUT: Vec<u8> = Vec::new();

/// Puts `s` in the output buffer as a u32 LE length + UTF-8 and returns a pointer to it.
fn output(text: &str) -> *const u8 {
    let ptr = &raw mut OUTPUT;
    // SAFETY: rules 1 and 3 of the module state; nothing else holds a reference to OUTPUT.
    let out = unsafe { &mut *ptr };
    out.clear();
    out.extend_from_slice(&(text.len() as u32).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
    out.as_ptr()
}

fn push_json_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                out.push_str("\\u00");
                out.push(HEX[c as usize >> 4] as char);
                out.push(HEX[c as usize & 15] as char);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}
fn push_json_array<T: AsRef<str>>(out: &mut String, items: &[T]) {
    out.push('[');
    for (i, s) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        push_json_string(out, s.as_ref());
    }
    out.push(']');
}
fn push_number(out: &mut String, value: usize) {
    out.push_str(&value.to_string());
}

// ---------- character classes of the reference regexes ----------
/// LET = A-Za-zÀ-ÖØ-öø-ÿ
fn is_letter(c: char) -> bool {
    matches!(c, 'A'..='Z' | 'a'..='z' | '\u{c0}'..='\u{d6}' | '\u{d8}'..='\u{f6}' | '\u{f8}'..='\u{ff}')
}
/// JS \s
fn is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}
/// The apostrophe and what people type in its place: ’ (typographic), ´ (a lone acute accent,
/// common on keyboards: "d´Stad"), ` (grave accent), ʼ (modifier letter), ‘ (opening quote).
fn is_apostrophe(c: char) -> bool {
    matches!(
        c,
        '\'' | '\u{2019}' | '\u{b4}' | '`' | '\u{2bc}' | '\u{2018}'
    )
}
/// UPPER = /^[A-ZÀ-ÖØ-Þ]/
fn starts_upper(text: &str) -> bool {
    matches!(
        text.chars().next(),
        Some('A'..='Z' | '\u{c0}'..='\u{d6}' | '\u{d8}'..='\u{de}')
    )
}
/// /^[ \t ]*$/
fn is_blank(chars: &[char]) -> bool {
    chars
        .iter()
        .all(|&c| c == ' ' || c == '\t' || c == '\u{a0}')
}
/// /^\s*["”“»’']*[.!?…]/
fn sentence_end(chars: &[char]) -> bool {
    let mut i = 0;
    while i < chars.len() && is_space(chars[i]) {
        i += 1;
    }
    while i < chars.len()
        && matches!(
            chars[i],
            '"' | '\u{201d}' | '\u{201c}' | '\u{bb}' | '\u{2019}' | '\''
        )
    {
        i += 1;
    }
    i < chars.len() && matches!(chars[i], '.' | '!' | '?' | '\u{2026}')
}
fn js_trim(text: &str) -> &str {
    text.trim_matches(is_space)
}
fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// charAt(0).toLowerCase() + slice(1). charAt works on UTF-16 units: a lone surrogate has no case.
fn lowercase_first(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(c) if (c as u32) <= 0xffff => c.to_lowercase().chain(chars).collect(),
        _ => word.to_string(),
    }
}
fn uppercase_first(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(c) if (c as u32) <= 0xffff => c.to_uppercase().chain(chars).collect(),
        _ => word.to_string(),
    }
}
fn keep_case(original: &str, replacement: &str) -> String {
    if starts_upper(original) && !starts_upper(replacement) {
        uppercase_first(replacement)
    } else {
        replacement.to_string()
    }
}

const N_KEEP_INITIALS: &str = "aeiouäëéèêöüàâîôûndtzh";
const N_DROP_INITIALS: &str = "bcfgjklmpqrsvwx";
/// String.prototype.indexOf(x) >= 0 (true for the empty string, like the reference).
fn in_set(set: &str, text: &str) -> bool {
    set.contains(text)
}
/// Whether the next word keeps a final n before it: Some(true) keep, Some(false) drop, None
/// when the rule does not decide (an unknown Y word, a digit…). Y depends on how it sounds:
/// "en Yttrium" (vowel) but "e Yoga" (the consonant [j]), taken from the LOD pronunciation.
fn keeps_n_before(dictionary: &Dictionary, next: &Word, next_initial: &str) -> Option<bool> {
    // The LOD's pronunciation first, where it differs from the spelling (Job, Gin, Yoga, User):
    // only when every entry of the word agrees.
    // The sound is the lemma's: a form that does not start like it ("gegendert" for "genderen")
    // keeps to its letters.
    let start: String = next.normalized.to_lowercase().chars().take(3).collect();
    let entries = dictionary.entry_indices(next.record);
    let onsets: Vec<u8> = entries
        .iter()
        .map(|&i| &dictionary.entries[i])
        .filter(|entry| entry.lemma.to_lowercase().starts_with(&start))
        .map(|entry| entry.y_onset)
        .collect();
    if let Some(&first) = onsets.first() {
        if first != 0 && onsets.iter().all(|&onset| onset == first) {
            return Some(first == b'v');
        }
    }
    if next_initial == "y" {
        return None;
    }
    if !next_initial.is_empty() && in_set(N_KEEP_INITIALS, next_initial) {
        Some(true)
    } else if in_set(N_DROP_INITIALS, next_initial) {
        Some(false)
    } else {
        None
    }
}

/// "den|de" style variants: the first keeps the n, the next ones drop it.
fn pick_variant(variants: &str, keeps_n: bool) -> &str {
    match variants.split_once('|') {
        None => variants,
        Some((with_n, without_n)) => {
            if keeps_n {
                with_n
            } else {
                without_n.split('|').next().unwrap_or("")
            }
        }
    }
}

/// The clitics the engine knows ("s" of the 2nd person, "d'", "z'", "'t", "'n", "'s"): short forms
/// that are never checked on their own. The product says what each one is.
const CLITICS: [&str; 6] = ["s", "d'", "z'", "'t", "'n", "'s"];
fn is_clitic(word: &str) -> bool {
    CLITICS.contains(&word)
}
fn is_s_function_word(word: &str) -> bool {
    matches!(
        word,
        // "s" is the clitic of the 2nd person: the LOD always keeps the n ("wann s de", 351
        // examples). Before "seng…", "säin", "sech", "si" it writes both ("a sengem" 94, "an
        // sengem" 13): the rule stays silent rather than flag a form the LOD uses.
        "s" | "si"
            | "se"
            | "sech"
            | "säin"
            | "säi"
            | "seng"
            | "senge"
            | "sengem"
            | "senger"
            | "sengen"
            | "sou"
    )
}
fn is_polite_form(word: &str) -> bool {
    matches!(
        word,
        "Dir" | "Iech" | "Är" | "Ärem" | "Ärer" | "Ären" | "Äert"
    )
}
fn forced_full_form(form: &str) -> Option<&'static str> {
    match form {
        "a" => Some("an"),
        "de" => Some("den"),
        _ => None,
    }
}

fn gender_index(gender: char) -> usize {
    match gender {
        'M' => 0,
        'F' => 1,
        _ => 2,
    }
}
fn determiner(genders: StaticStr, by_gender: &[(char, &str)]) -> Determiner {
    let mut result = Determiner {
        genders,
        by_gender: [None, None, None],
        plural: None,
    };
    for &(gender, alternative) in by_gender {
        result.by_gender[gender_index(gender)] = Some(alternative.to_string());
    }
    result
}
/// Same construction order as the reference so later assignments overwrite earlier ones identically.
fn build_determiners() -> HashMap<String, Determiner> {
    let mut table: HashMap<String, Determiner> = HashMap::new();
    let put = |table: &mut HashMap<String, Determiner>, word: &str, entry: Determiner| {
        table.insert(word.to_string(), entry);
    };
    put(
        &mut table,
        "den",
        determiner("M", &[('F', "d'"), ('N', "d'")]),
    );
    put(
        &mut table,
        "de",
        determiner("M", &[('F', "d'"), ('N', "d'")]),
    );
    put(&mut table, "d'", determiner("FN", &[('M', "den|de")]));
    put(&mut table, "en", determiner("MN", &[('F', "eng")]));
    put(&mut table, "e", determiner("MN", &[('F', "eng")]));
    // "een" / "ee" (one, a): masculine / neuter like "en" / "e"; feminine is "eng" ("ee Fro" → "eng Fro")
    put(&mut table, "een", determiner("MN", &[('F', "eng")]));
    put(&mut table, "ee", determiner("MN", &[('F', "eng")]));
    put(
        &mut table,
        "eng",
        determiner("F", &[('M', "en|e"), ('N', "en|e")]),
    );
    put(&mut table, "dem", determiner("MN", &[('F', "der")]));
    put(
        &mut table,
        "der",
        determiner("F", &[('M', "dem"), ('N', "dem")]),
    );
    put(&mut table, "engem", determiner("MN", &[('F', "enger")]));
    put(
        &mut table,
        "enger",
        determiner("F", &[('M', "engem"), ('N', "engem")]),
    );
    put(&mut table, "am", determiner("MN", &[('F', "an der")]));
    put(&mut table, "um", determiner("MN", &[('F', "op der")]));
    put(&mut table, "vum", determiner("MN", &[('F', "vun der")]));
    put(&mut table, "zum", determiner("MN", &[('F', "zur")]));
    put(&mut table, "beim", determiner("MN", &[('F', "bei der")]));
    put(
        &mut table,
        "zur",
        determiner("F", &[('M', "zum"), ('N', "zum")]),
    );
    // [masculine (with n-rule variant), feminine, neuter] — nominative/accusative
    let groups: [(&str, &str, Option<&str>); 8] = [
        ("mäin|mäi", "meng", None),
        ("däin|däi", "deng", None),
        ("säin|säi", "seng", None),
        ("keen|kee", "keng", None),
        ("hiren|hire", "hir", Some("hiert")),
        ("eisen|eise", "eis", Some("eist")),
        ("ären|äre", "är", Some("äert")),
        ("dësen|dëse", "dës", Some("dëst")),
    ];
    for (masculine, feminine, neuter) in groups {
        let neuter_or_masculine = neuter.unwrap_or(masculine);
        for form in masculine.split('|') {
            let mut entry = match neuter {
                Some(neuter) => determiner("M", &[('F', feminine), ('N', neuter)]),
                None => determiner("MN", &[('F', feminine)]),
            };
            // "hiren / hire", "eisen / eise", "ären / äre", "dësen / dëse" are also the dative
            // plural ("mat hire Kanner", "vun eise Clienten"): no plural check without the case.
            if neuter.is_none() {
                entry.plural = Some(feminine.to_string());
            }
            put(&mut table, form, entry);
        }
        put(
            &mut table,
            feminine,
            determiner("F", &[('M', masculine), ('N', neuter_or_masculine)]),
        );
        if let Some(neuter) = neuter {
            let mut entry = determiner("N", &[('M', masculine), ('F', feminine)]);
            entry.plural = Some(feminine.to_string());
            put(&mut table, neuter, entry);
        }
    }
    // dative: -em (masculine, neuter) / -er (feminine)
    for stem in ["meng", "deng", "seng", "keng", "hir", "eis", "är", "dës"] {
        let (dative_masculine, dative_feminine) = (format!("{stem}em"), format!("{stem}er"));
        let mut entry = determiner("MN", &[('F', &dative_feminine)]);
        entry.plural = Some(format!("{stem}en|{stem}e"));
        put(&mut table, &dative_masculine, entry);
        put(
            &mut table,
            &dative_feminine,
            determiner("F", &[('M', &dative_masculine), ('N', &dative_masculine)]),
        );
    }
    // contracted preposition + dem: plural needs "den"
    for (contracted, preposition) in [
        ("am", "an"),
        ("um", "op"),
        ("vum", "vun"),
        ("zum", "zu"),
        ("beim", "bei"),
    ] {
        table
            .get_mut(contracted)
            .expect("every contracted form is in the table above")
            .plural = Some(format!("{preposition} den|{preposition} de"));
    }
    table
}

// ---------- data ----------
fn load_dictionary(text: StaticStr) -> Result<Dictionary, i32> {
    let mut lines = text.split('\n');
    let head: Vec<StaticStr> = lines.next().ok_or(2)?.split('\t').collect();
    if head.len() != 10 || head[0] != "LETZ3" {
        return Err(2);
    }
    let count = |field: usize| head[field].parse::<usize>().map_err(|_| 2);
    let (entry_count, form_count, attested_count, mask_count, conjugation_count) =
        (count(2)?, count(3)?, count(4)?, count(5)?, count(6)?);

    let mut entries = Vec::with_capacity(entry_count);
    for _ in 0..entry_count {
        let fields: Vec<StaticStr> = lines.next().ok_or(3)?.split('\t').collect();
        if fields.len() != 11 {
            return Err(3);
        }
        entries.push(Entry {
            id: fields[0],
            lemma: fields[1],
            part_of_speech: fields[2],
            gender: fields[3],
            level: fields[4].parse().map_err(|_| 3)?,
            translations: [fields[5], fields[6], fields[9], fields[10]],
            plural: fields[7],
            y_onset: match fields[8] {
                "j" => b'j',
                "v" => b'v',
                _ => 0,
            },
        });
    }
    let mut forms: Map<StaticStr, FormRecord> = Map::default();
    forms.reserve(form_count);
    let mut n_dropped: Map<StaticStr, StaticStr> = Map::default();
    for _ in 0..form_count {
        let fields: Vec<StaticStr> = lines.next().ok_or(4)?.split('\t').collect();
        if fields.len() != 4 {
            return Err(4);
        }
        forms.insert(
            fields[0],
            FormRecord {
                entry_ids: fields[1],
                full_forms: fields[2],
                only_n_variant: fields[3] == "n",
            },
        );
        if !fields[2].is_empty() {
            for full in fields[2].split('|') {
                n_dropped.entry(full).or_insert(fields[0]);
            }
        }
    }
    let mut attested: Set<StaticStr> = Set::default();
    attested.reserve(attested_count);
    for _ in 0..attested_count {
        attested.insert(lines.next().ok_or(5)?);
    }
    let mut person_masks: Map<StaticStr, (u32, bool)> = Map::default();
    person_masks.reserve(mask_count);
    for _ in 0..mask_count {
        let (form, mask) = lines.next().ok_or(6)?.split_once('\t').ok_or(6)?;
        let wild = mask.ends_with('w');
        let digits = mask.trim_end_matches('w');
        person_masks.insert(form, (digits.parse().map_err(|_| 6)?, wild));
    }
    let mut conjugations: Map<u32, [StaticStr; 8]> = Map::default();
    conjugations.reserve(conjugation_count);
    for _ in 0..conjugation_count {
        let fields: Vec<StaticStr> = lines.next().ok_or(7)?.split('\t').collect();
        if fields.len() != 9 {
            return Err(7);
        }
        conjugations.insert(
            fields[0].parse().map_err(|_| 7)?,
            [
                fields[1], fields[2], fields[3], fields[4], fields[5], fields[6], fields[7],
                fields[8],
            ],
        );
    }
    let (affix_count, word_count) = (count(7)?, count(8)?);
    let affix_lines: Vec<StaticStr> = lines.by_ref().take(affix_count).collect();
    let word_lines: Vec<StaticStr> = lines.by_ref().take(word_count).collect();
    if affix_lines.len() != affix_count || word_lines.len() != word_count {
        return Err(8);
    }
    let speller = if word_count == 0 {
        spell::Speller::empty()
    } else {
        spell::Speller::load(head[9], &affix_lines, &word_lines).ok_or(8)?
    };
    let mut nfd: Map<char, StaticStr> = Map::default();
    for line in include_str!("nfd.txt").lines() {
        let mut chars = line.chars();
        if let Some(character) = chars.next() {
            nfd.insert(character, chars.as_str());
        }
    }
    Ok(Dictionary {
        version: head[1],
        entries,
        forms,
        n_dropped,
        attested,
        person_masks,
        conjugations,
        nfd,
        determiners: build_determiners(),
        speller,
        correction_cache: std::cell::RefCell::new(Map::default()),
    })
}

impl Dictionary {
    fn valid_record(&self, form: &str) -> Option<FormRecord> {
        self.forms
            .get(form)
            .copied()
            .filter(|record| !record.entry_ids.starts_with('!'))
    }
    fn lookup_any_case(&self, word: &str) -> Option<(String, FormRecord)> {
        for candidate in [
            word.to_string(),
            lowercase_first(word),
            uppercase_first(word),
        ] {
            if let Some(record) = self.valid_record(&candidate) {
                return Some((candidate, record));
            }
        }
        None
    }
    /// Entry indices of a record; for an n-rule form the entries of its full form come first.
    fn entry_indices(&self, record: Option<FormRecord>) -> Vec<usize> {
        let Some(record) = record else {
            return Vec::new();
        };
        if record.entry_ids.is_empty() {
            return Vec::new();
        }
        let ids = record
            .entry_ids
            .strip_prefix('!')
            .unwrap_or(record.entry_ids);
        let mut list: Vec<usize> = ids
            .split(',')
            .filter_map(|id| id.parse::<usize>().ok())
            .filter(|&i| i < self.entries.len())
            .collect();
        if !record.full_forms.is_empty() {
            let is_full = |i: &usize| {
                record
                    .full_forms
                    .split('|')
                    .any(|full| full == self.entries[*i].lemma)
            };
            let (mut first, rest): (Vec<usize>, Vec<usize>) = list.iter().partition(|i| is_full(i));
            first.extend(rest);
            list = first;
        }
        list
    }
    /// The first translation of the word's main entry, in LANGUAGES[language].
    fn gloss(&self, record: Option<FormRecord>, language: usize) -> &'static str {
        let indices = self.entry_indices(record);
        let translation = |i: usize| self.entries[i].translations[language];
        for pass in 0..2 {
            for &i in &indices {
                if (self.entries[i].part_of_speech == "ART") != (pass == 0) {
                    continue;
                }
                let text = translation(i);
                if !text.is_empty() {
                    return js_trim(text.split(';').next().unwrap_or(""));
                }
            }
        }
        ""
    }
    fn level(&self, record: Option<FormRecord>) -> u8 {
        let mut best = 0;
        for i in self.entry_indices(record) {
            let entry_level = self.entries[i].level;
            if entry_level != 0 && (best == 0 || entry_level < best) {
                best = entry_level;
            }
        }
        best
    }

    // ---------- suggestions ----------
    fn push_nfd(&self, c: char, out: &mut Vec<u16>) {
        let code = c as u32;
        let mut buf = [0u16; 2];
        if (0xac00..=0xd7a3).contains(&code) {
            let syllable = code - 0xac00;
            out.push((0x1100 + syllable / 588) as u16);
            out.push((0x1161 + (syllable % 588) / 28) as u16);
            if !syllable.is_multiple_of(28) {
                out.push((0x11a7 + syllable % 28) as u16);
            }
        } else if let Some(decomposition) = if code < 0xc0 { None } else { self.nfd.get(&c) } {
            out.extend(decomposition.encode_utf16());
        } else {
            out.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    /// s.toLowerCase().normalize('NFD') minus combining marks, apostrophes and hyphens,
    /// with runs of one repeated UTF-16 unit collapsed.
    fn fold(&self, text: &str) -> Vec<u16> {
        let lowercase = text.to_lowercase();
        let mut out: Vec<u16> = Vec::with_capacity(lowercase.len());
        let mut decomposed: Vec<u16> = Vec::with_capacity(4);
        for c in lowercase.chars() {
            decomposed.clear();
            self.push_nfd(c, &mut decomposed);
            for &u in &decomposed {
                if (0x300..=0x36f).contains(&u)
                    || u == 0x2d
                    || char::from_u32(u32::from(u)).is_some_and(is_apostrophe)
                {
                    continue;
                }
                push_collapsed(&mut out, u);
            }
        }
        out
    }
    /// Lower case without combining marks (é → e, ë → e); keeps everything else, unlike fold().
    fn no_marks(&self, text: &str) -> Vec<u16> {
        let mut out = Vec::with_capacity(text.len());
        for c in text.to_lowercase().chars() {
            let mut decomposed = Vec::with_capacity(4);
            self.push_nfd(c, &mut decomposed);
            out.extend(
                decomposed
                    .into_iter()
                    .filter(|u| !(0x300..=0x36f).contains(u)),
            );
        }
        out
    }
    fn skeleton(&self, text: &str) -> Vec<u16> {
        let mut folded = self.fold(text);
        folded.retain(|&u| !matches!(u, 0x61 | 0x65 | 0x69 | 0x6f | 0x75 | 0x79));
        folded
    }
    /// Folded spelling -> forms, built on first use (only the suggestions need it).
    fn fold_index(&self) -> &'static Map<Vec<u16>, Vec<StaticStr>> {
        let ptr = &raw mut FOLD_INDEX;
        // SAFETY: rules 1 and 2 of the module state. FOLD_INDEX is only reset by `init`, which never
        // runs while this reference is alive.
        let slot = unsafe { &mut *ptr };
        slot.get_or_insert_with(|| {
            let mut index: Map<Vec<u16>, Vec<StaticStr>> = Map::default();
            for (form, rec) in &self.forms {
                if rec.entry_ids.starts_with('!') || rec.only_n_variant {
                    continue;
                }
                index.entry(self.fold(form)).or_default().push(form);
            }
            index
        })
    }
    fn suggest(&self, word: &str, max: usize) -> Vec<StaticStr> {
        let mut candidates = self.lod_candidates(word);
        candidates.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.encode_utf16().cmp(b.1.encode_utf16()))
        });
        candidates.truncate(if max == 0 { 4 } else { max });
        candidates.into_iter().map(|x| x.1).collect()
    }

    /// Corrections for a misspelt word: LOD forms close to it (scored as in `suggest`) and the
    /// spellings the spellchecker.lu dictionary accepts one typo away, ranked together. Typical
    /// confusions from that dictionary (REP), two swapped letters and a neighbouring key on the
    /// keyboard count as smaller mistakes than their edit distance says.
    fn suggest_spelling(&self, word: &str, max: usize) -> Vec<String> {
        const LEFT_OUT_BONUS: i32 = -8;
        let speller = &self.speller;
        let replacements = speller.replacement_candidates(word);
        let word_lowercase: Vec<u16> = word.to_lowercase().encode_utf16().collect();
        let word_characters: Vec<char> = word.chars().collect();
        let capital = starts_upper(word);
        let skeleton = self.skeleton(word);
        let word_length = utf16_len(word) as i32;
        let bonus = |candidate: &str| -> i32 {
            let characters: Vec<char> = candidate.chars().collect();
            if replacements
                .iter()
                .any(|replacement| replacement == candidate)
            {
                return -8;
            }
            // A letter left out, the most common typo: the candidate is the word plus one letter.
            if characters.len() == word_characters.len() + 1 {
                let mut skipped = false;
                let mut at = 0;
                for &c in &characters {
                    if at < word_characters.len() && word_characters[at] == c {
                        at += 1;
                    } else if !skipped {
                        skipped = true;
                    } else {
                        return 0;
                    }
                }
                return if at == word_characters.len() {
                    LEFT_OUT_BONUS
                } else {
                    0
                };
            }
            if characters.len() != word_characters.len() {
                return 0;
            }
            let differences: Vec<usize> = (0..characters.len())
                .filter(|&i| characters[i] != word_characters[i])
                .collect();
            match differences.as_slice() {
                [first, second]
                    if *second == first + 1
                        && characters[*first] == word_characters[*second]
                        && characters[*second] == word_characters[*first] =>
                {
                    -8
                }
                [only]
                    if speller.keyboard_neighbours(characters[*only], word_characters[*only]) =>
                {
                    -3
                }
                _ => 0,
            }
        };
        let mut scored: Vec<(i32, String)> = self
            .lod_candidates(word)
            .into_iter()
            .map(|(score, form)| (score + bonus(form), form.to_string()))
            .collect();
        for candidate in self.speller_corrections(word) {
            if scored.iter().any(|(_, known)| *known == candidate) {
                continue;
            }
            let lowercase: Vec<u16> = candidate.to_lowercase().encode_utf16().collect();
            let distance = edit_distance(&word_lowercase, &lowercase) as i32;
            // Scored like a LOD form without a level (+3): no translation to show, but the
            // dictionary says it is a real spelling.
            let score = distance * 10
                - if self.skeleton(&candidate) == skeleton {
                    6
                } else {
                    0
                }
                + if starts_upper(&candidate) == capital {
                    0
                } else {
                    2
                }
                + 3
                + (utf16_len(&candidate) as i32 - word_length).abs()
                + bonus(&candidate);
            scored.push((score, candidate));
        }
        scored.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.encode_utf16().cmp(b.1.encode_utf16()))
        });
        scored.truncate(if max == 0 { 4 } else { max });
        scored.into_iter().map(|x| x.1).collect()
    }

    /// `speller.corrections(word)`, remembered (up to a few thousand words, then forgotten).
    fn speller_corrections(&self, word: &str) -> Vec<String> {
        const REMEMBERED: usize = 4000;
        if let Some(known) = self.correction_cache.borrow().get(word) {
            return known.clone();
        }
        let found = self.speller.corrections(word);
        let mut cache = self.correction_cache.borrow_mut();
        if cache.len() >= REMEMBERED {
            cache.clear();
        }
        cache.insert(word.to_string(), found.clone());
        found
    }

    /// LOD forms close to `word`, with their score (lower is better), unsorted.
    fn lod_candidates(&self, word: &str) -> Vec<(i32, StaticStr)> {
        let index = self.fold_index();
        let base = self.fold(word);
        let length = base.len();
        let mut keys: Vec<Vec<u16>> = vec![base.clone()];
        for i in 0..=length {
            for c in b'a'..=b'z' {
                let mut key = Vec::with_capacity(length + 1);
                key.extend_from_slice(&base[..i]);
                key.push(c as u16);
                key.extend_from_slice(&base[i..]);
                keys.push(key);
                if i < length {
                    let mut key = base.clone();
                    key[i] = c as u16;
                    keys.push(key);
                }
            }
            if i < length {
                let mut key = base.clone();
                key.remove(i);
                keys.push(key);
            }
            if i + 1 < length {
                let mut key = base.clone();
                key.swap(i, i + 1);
                keys.push(key);
            }
        }
        let lowercase: Vec<u16> = word.to_lowercase().encode_utf16().collect();
        let capital = starts_upper(word);
        let skeleton = self.skeleton(word);
        let word_length = utf16_len(word) as i32;
        let mut seen: Set<StaticStr> = Set::default();
        let mut candidates: Vec<(i32, StaticStr)> = Vec::new();
        for key in &keys {
            let mut collapsed: Vec<u16> = Vec::with_capacity(key.len());
            for &u in key {
                push_collapsed(&mut collapsed, u);
            }
            let Some(list) = index.get(&collapsed) else {
                continue;
            };
            for &form in list {
                if form == word || !seen.insert(form) {
                    continue;
                }
                let form_lowercase: Vec<u16> = form.to_lowercase().encode_utf16().collect();
                let distance = edit_distance(&lowercase, &form_lowercase) as i32;
                if distance > 3 {
                    continue;
                }
                let form_level = self.level(self.forms.get(form).copied());
                let score = distance * 10
                    - if self.skeleton(form) == skeleton {
                        6
                    } else {
                        0
                    }
                    + if starts_upper(form) == capital { 0 } else { 2 }
                    + match form_level {
                        1 => 0,
                        2 => 1,
                        _ => 3,
                    }
                    + (utf16_len(form) as i32 - word_length).abs();
                candidates.push((score, form));
            }
        }
        candidates
    }
    fn split_compound(&self, word: &str) -> Option<(String, FormRecord, String, FormRecord)> {
        let chars: Vec<char> = word.chars().collect();
        if chars.len() < 7 {
            return None;
        }
        let has_part_of_speech = |record: FormRecord, parts_of_speech: &[&str]| {
            self.entry_indices(Some(record))
                .iter()
                .any(|&i| parts_of_speech.contains(&self.entries[i].part_of_speech))
        };
        let mut best: Option<(usize, (String, FormRecord, String, FormRecord))> = None;
        for i in 3..=chars.len() - 3 {
            let first: String = chars[..i].iter().collect();
            let second: String = chars[i..].iter().collect();
            let first_found = self.lookup_any_case(&first).or_else(|| {
                if first.ends_with('s') && i > 3 {
                    self.lookup_any_case(&chars[..i - 1].iter().collect::<String>())
                } else {
                    None
                }
            });
            let second_found = self.lookup_any_case(&second);
            let (Some(ra), Some(rb)) = (first_found, second_found) else {
                continue;
            };
            if !has_part_of_speech(ra.1, &["SUBST", "ADJ", "VRB", "NB", "ADV"])
                || !has_part_of_speech(rb.1, &["SUBST", "ADJ", "VRB"])
            {
                continue;
            }
            let shorter = i.min(chars.len() - i);
            if best.as_ref().is_none_or(|current| shorter > current.0) {
                best = Some((shorter, (ra.0, ra.1, rb.0, rb.1)));
            }
        }
        best.map(|b| b.1)
    }
}

/// `(.)\1+` → `$1` as a stream: `.` does not match line terminators, so those never collapse.
fn push_collapsed(out: &mut Vec<u16>, unit: u16) {
    if out.last() == Some(&unit) && !matches!(unit, 0x0a | 0x0d | 0x2028 | 0x2029) {
        return;
    }
    out.push(unit);
}

/// Optimal string alignment distance over UTF-16 units.
#[allow(clippy::needless_range_loop)] // the indices follow the textbook recurrence
fn edit_distance(a: &[u16], b: &[u16]) -> usize {
    let (n, m) = (a.len(), b.len());
    let width = m + 1;
    let mut table = vec![0usize; (n + 1) * width];
    for i in 0..=n {
        table[i * width] = i;
    }
    for j in 1..=m {
        table[j] = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            let mut value = (table[(i - 1) * width + j] + 1)
                .min(table[i * width + j - 1] + 1)
                .min(table[(i - 1) * width + j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                value = value.min(table[(i - 2) * width + j - 2] + 1);
            }
            table[i * width + j] = value;
        }
    }
    table[n * width + m]
}

// ---------- tokenizer ----------
#[derive(Clone, Copy, PartialEq)]
enum TokenKind {
    Word,
    Space,
    Punctuation,
    Number,
}
struct Token {
    start: usize,
    end: usize,
    kind: TokenKind,
}

fn lowercase_char(c: char) -> String {
    c.to_lowercase().collect()
}

fn tokenize(dictionary: &Dictionary, chars: &[char]) -> Vec<Token> {
    let length = chars.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < length {
        let c = chars[i];
        let mut j = i;
        let word = if is_letter(c) {
            // [L]+(?:['’\-][L]+)*['’]?
            while j < length && is_letter(chars[j]) {
                j += 1;
            }
            while j + 1 < length
                && (is_apostrophe(chars[j]) || chars[j] == '-')
                && is_letter(chars[j + 1])
            {
                j += 1;
                while j < length && is_letter(chars[j]) {
                    j += 1;
                }
            }
            if j < length && is_apostrophe(chars[j]) {
                j += 1;
            }
            true
        } else if is_apostrophe(c) && i + 1 < length && is_letter(chars[i + 1]) {
            // ['’][L]+
            j = i + 1;
            while j < length && is_letter(chars[j]) {
                j += 1;
            }
            true
        } else {
            false
        };
        if !word {
            let kind = if c.is_ascii_digit() {
                // \d+(?:[.,]\d+)*
                while j < length && chars[j].is_ascii_digit() {
                    j += 1;
                }
                while j + 1 < length
                    && (chars[j] == '.' || chars[j] == ',')
                    && chars[j + 1].is_ascii_digit()
                {
                    j += 1;
                    while j < length && chars[j].is_ascii_digit() {
                        j += 1;
                    }
                }
                TokenKind::Number
            } else if is_space(c) {
                while j < length && is_space(chars[j]) {
                    j += 1;
                }
                TokenKind::Space
            } else {
                j = i + 1;
                TokenKind::Punctuation
            };
            out.push(Token {
                start: i,
                end: j,
                kind,
            });
            i = j;
            continue;
        }
        let has_apostrophe = chars[i..j].iter().any(|&c| is_apostrophe(c));
        if !has_apostrophe
            || dictionary
                .valid_record(&normalize_apostrophes(&chars[i..j]))
                .is_some()
        {
            out.push(Token {
                start: i,
                end: j,
                kind: TokenKind::Word,
            });
            i = j;
            continue;
        }
        // split clitics: d'Zopp → d' + Zopp ; wéi's → wéi + 's ; 'Hond' → Hond + '
        let mut position = i;
        while position < j {
            let rest = &chars[position..j];
            let Some(apostrophe) = rest.iter().position(|&c| is_apostrophe(c)) else {
                out.push(Token {
                    start: position,
                    end: j,
                    kind: TokenKind::Word,
                });
                break;
            };
            let after = &rest[apostrophe + 1..];
            let after_clitic =
                after.len() == 1 && is_clitic(&format!("'{}", lowercase_char(after[0])));
            if apostrophe == 0 {
                if after_clitic {
                    out.push(Token {
                        start: position,
                        end: j,
                        kind: TokenKind::Word,
                    });
                    break;
                }
                out.push(Token {
                    start: position,
                    end: position + 1,
                    kind: TokenKind::Punctuation,
                });
                position += 1;
                continue;
            }
            if apostrophe == 1 && is_clitic(&format!("{}'", lowercase_char(rest[0]))) {
                out.push(Token {
                    start: position,
                    end: position + 2,
                    kind: TokenKind::Word,
                });
                position += 2;
                continue;
            }
            out.push(Token {
                start: position,
                end: position + apostrophe,
                kind: TokenKind::Word,
            });
            position += apostrophe;
            if after_clitic {
                out.push(Token {
                    start: position,
                    end: j,
                    kind: TokenKind::Word,
                });
                break;
            }
        }
        i = j;
    }
    out
}

/// Token text with the typographic apostrophe normalised.
fn normalize_apostrophes(chars: &[char]) -> String {
    chars
        .iter()
        .map(|&c| if is_apostrophe(c) { '\'' } else { c })
        .collect()
}

// ---------- numbers in words ----------
// Luxembourgish writes a number below a million in one word: "néngavéierzeg" (9 + a + 40),
// "honnertsiwwenzéngdausendsechshonnertnéngavéierzeg" (117,649), years as hundreds
// ("uechtzénghonnertfofzéng", 1815). Millioun / Milliounen is a noun and stands apart
// ("honnertdräianzwanzeg Milliounen …"). The LOD lists the pieces (and a few compounds such as
// "eenanzwanzeg") but cannot list every number, so the pieces are written out here; every one of
// them is a LOD form.
const NUMBER_UNITS: [(&str, u64); 10] = [
    ("een", 1),
    ("eent", 1),
    ("zwee", 2),
    ("dräi", 3),
    ("véier", 4),
    ("fënnef", 5),
    ("sechs", 6),
    ("siwen", 7),
    ("aacht", 8),
    ("néng", 9),
];
const NUMBER_TEENS: [(&str, u64); 10] = [
    ("zéng", 10),
    ("eelef", 11),
    ("zwielef", 12),
    ("dräizéng", 13),
    ("véierzéng", 14),
    ("fofzéng", 15),
    ("siechzéng", 16),
    ("siwwenzéng", 17),
    ("uechtzéng", 18),
    ("nonzéng", 19),
];
const NUMBER_TENS: [(&str, u64); 10] = [
    ("zwanzeg", 20),
    ("drësseg", 30),
    ("véierzeg", 40),
    ("fofzeg", 50),
    ("sechzeg", 60),
    ("siechzeg", 60),
    ("siwwenzeg", 70),
    ("achtzeg", 80),
    ("uechtzeg", 80),
    ("nonzeg", 90),
];

/// Misspellings of a piece that are common enough to read anyway, with the right piece:
/// "aachtzeg" for "achtzeg" (80), by analogy with "aacht" (8).
const NUMBER_MISSPELLINGS: [(&str, &str); 1] = [("aachtzeg", "achtzeg")];

/// A number read from a word: its value and its spelling with the right accents.
struct NumberReading {
    value: u64,
    spelling: String,
}

/// Lower case without accents, to read a number whatever its accents ("uechtzeng").
fn fold_number(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| match c {
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'ä' | 'à' | 'â' => 'a',
            'ö' | 'ô' => 'o',
            'ü' | 'û' => 'u',
            'î' | 'ï' => 'i',
            _ => c,
        })
        .collect()
}

/// `text` (folded) is exactly one of the pieces: its value and spelling.
fn number_piece(text: &str, pieces: &[(&'static str, u64)]) -> Option<NumberReading> {
    let text = NUMBER_MISSPELLINGS
        .iter()
        .find(|(wrong, _)| *wrong == text)
        .map_or(text, |&(_, right)| right);
    pieces
        .iter()
        .find(|(piece, _)| fold_number(piece) == text)
        .map(|&(piece, value)| NumberReading {
            value,
            spelling: piece.to_string(),
        })
}

/// 1 to 99: a unit, a teen, a ten, or unit + "a"/"an" + ten ("zweeasechzeg", "eenanzwanzeg").
/// Both linking forms are read: their choice follows the n-rule, which is not checked here.
fn number_below_hundred(text: &str) -> Option<NumberReading> {
    if let Some(found) = number_piece(text, &NUMBER_UNITS)
        .or_else(|| number_piece(text, &NUMBER_TEENS))
        .or_else(|| number_piece(text, &NUMBER_TENS))
    {
        return Some(found);
    }
    for &(unit, unit_value) in &NUMBER_UNITS {
        let Some(rest) = text.strip_prefix(fold_number(unit).as_str()) else {
            continue;
        };
        for link in ["an", "a"] {
            if let Some(ten) = rest
                .strip_prefix(link)
                .and_then(|r| number_piece(r, &NUMBER_TENS))
            {
                return Some(NumberReading {
                    value: unit_value + ten.value,
                    spelling: format!("{unit}{link}{}", ten.spelling),
                });
            }
        }
    }
    None
}

/// `text` split at `keyword` (folded): what comes before and after it.
fn split_at_keyword<'a>(text: &'a str, keyword: &str) -> Option<(&'a str, &'a str)> {
    let at = text.find(keyword)?;
    Some((&text[..at], &text[at + keyword.len()..]))
}

/// 1 to 9,999: below a hundred, or [unit or teen] "honnert" [below a hundred]; a teen before
/// "honnert" is how years are said ("uechtzénghonnertfofzéng", 1815).
fn number_below_thousand(text: &str) -> Option<NumberReading> {
    if let Some(found) = number_below_hundred(text) {
        return Some(found);
    }
    let (before, after) = split_at_keyword(text, "honnert")?;
    let hundreds = if before.is_empty() {
        NumberReading {
            value: 1,
            spelling: String::new(),
        }
    } else {
        number_piece(before, &NUMBER_UNITS).or_else(|| number_piece(before, &NUMBER_TEENS))?
    };
    let rest = if after.is_empty() {
        NumberReading {
            value: 0,
            spelling: String::new(),
        }
    } else {
        number_below_hundred(after)?
    };
    Some(NumberReading {
        value: hundreds.value * 100 + rest.value,
        spelling: format!("{}honnert{}", hundreds.spelling, rest.spelling),
    })
}

/// Below a million: [below a thousand] "dausend" [below a thousand], or below a thousand.
fn number_below_million(text: &str) -> Option<NumberReading> {
    if let Some(found) = number_below_thousand(text) {
        return Some(found);
    }
    let (before, after) = split_at_keyword(text, "dausend")?;
    let thousands = if before.is_empty() {
        NumberReading {
            value: 1,
            spelling: String::new(),
        }
    } else {
        number_below_thousand(before)?
    };
    let rest = if after.is_empty() {
        NumberReading {
            value: 0,
            spelling: String::new(),
        }
    } else {
        number_below_thousand(after)?
    };
    Some(NumberReading {
        value: thousands.value * 1000 + rest.value,
        spelling: format!("{}dausend{}", thousands.spelling, rest.spelling),
    })
}

/// A number written in one word, whatever its accents. The spelling is the right one: the word
/// with its accents, or, for millions written together ("…milliounvéierhonnert…"), the separate
/// form ("… Milliounen véierhonnert…"). Single pieces ("zwee", "honnert") are LOD words already.
fn read_number(word: &str) -> Option<NumberReading> {
    let text = fold_number(word);
    if let Some(found) = number_below_million(&text) {
        return Some(found);
    }
    let keyword = if text.contains("milliounen") {
        "milliounen"
    } else {
        "millioun"
    };
    let (before, after) = split_at_keyword(&text, keyword)?;
    let millions = if before.is_empty() {
        NumberReading {
            value: 1,
            spelling: "eng".to_string(),
        }
    } else {
        number_below_thousand(&before.replace("eng", "een"))?
    };
    let rest = if after.is_empty() {
        None
    } else {
        Some(number_below_million(after)?)
    };
    let noun = if millions.value == 1 {
        "Millioun"
    } else {
        "Milliounen"
    };
    let head = if millions.value == 1 {
        "eng".to_string()
    } else {
        millions.spelling
    };
    let mut spelling = format!("{head} {noun}");
    if let Some(rest) = &rest {
        spelling.push(' ');
        spelling.push_str(&rest.spelling);
    }
    Some(NumberReading {
        value: millions.value * 1_000_000 + rest.map_or(0, |rest| rest.value),
        spelling,
    })
}

// ---------- word classification ----------
#[derive(Clone, Copy, PartialEq)]
enum Status {
    Ok,
    Case,
    Wrong,
    Compound,
    Attested,
    Name,
    Unknown,
    /// No LOD entry, but the spellchecker.lu dictionary spells it this way.
    Speller,
    /// A number written in words, in one piece as Luxembourgish writes it
    /// ("aachtdausendzweehonnertzweeasechzeg").
    Number,
    /// A number written in words, but with wrong accents or millions written together.
    NumberToFix,
}

struct Word {
    start: usize,
    end: usize,
    text: String,
    normalized: String,
    status: Status,
    form: String,
    record: Option<FormRecord>,
    clitic: bool,
    part_records: Option<Vec<Option<FormRecord>>>,
    corrections: Vec<StaticStr>,
    compound: Option<(String, FormRecord, String, FormRecord)>,
    /// A number written in words: its value and its right spelling.
    number: Option<NumberReading>,
}

fn classify(dictionary: &Dictionary, chars: &[char], token: &Token, sentence_start: bool) -> Word {
    let text: String = chars[token.start..token.end].iter().collect();
    let normalized = normalize_apostrophes(&chars[token.start..token.end]);
    let mut word = Word {
        start: token.start,
        end: token.end,
        text,
        normalized: normalized.clone(),
        status: Status::Unknown,
        form: String::new(),
        record: None,
        clitic: false,
        part_records: None,
        corrections: Vec::new(),
        compound: None,
        number: None,
    };
    let resolve = |word: &mut Word, status: Status, form: String, record: Option<FormRecord>| {
        word.status = status;
        word.form = form;
        word.record = record;
    };
    let lowercase = normalized.to_lowercase();
    if is_clitic(&lowercase) {
        word.clitic = true;
        let record = dictionary.valid_record(&lowercase);
        resolve(&mut word, Status::Ok, lowercase, record);
        return word;
    }
    let capitalised = starts_upper(&normalized);
    let all_upper = normalized.chars().count() > 1 && normalized == normalized.to_uppercase();
    if let Some(record) = dictionary.valid_record(&normalized) {
        // sentence start: "Den" is the article, not the "Den" of "Den Haag"
        if capitalised && sentence_start {
            let lower = lowercase_first(&normalized);
            if let Some(lower_record) = dictionary.valid_record(&lower) {
                if !dictionary.entry_indices(Some(record)).iter().any(|&i| {
                    dictionary.entries[i].part_of_speech == "SUBST"
                        && dictionary.entries[i].lemma == normalized
                }) {
                    resolve(&mut word, Status::Ok, lower, Some(lower_record));
                    return word;
                }
            }
        }
        resolve(&mut word, Status::Ok, normalized, Some(record));
        return word;
    }
    if all_upper {
        let mut chars = normalized.chars();
        let first = chars
            .next()
            .expect("an all-upper-case word has at least two characters");
        let second: String = std::iter::once(first)
            .chain(chars.as_str().to_lowercase().chars())
            .collect();
        for candidate in [lowercase.clone(), second] {
            if let Some(record) = dictionary.valid_record(&candidate) {
                resolve(&mut word, Status::Ok, candidate, Some(record));
                return word;
            }
        }
    }
    if capitalised {
        let lower = lowercase_first(&normalized);
        if let Some(record) = dictionary.valid_record(&lower) {
            resolve(
                &mut word,
                if sentence_start || is_polite_form(&normalized) {
                    Status::Ok
                } else {
                    Status::Case
                },
                lower,
                Some(record),
            );
            return word;
        }
    } else {
        let upper = uppercase_first(&normalized);
        if let Some(record) = dictionary.valid_record(&upper) {
            resolve(&mut word, Status::Case, upper, Some(record));
            return word;
        }
    }
    if normalized.find('-').is_some_and(|i| i > 0) {
        // every part must be a known word (or a number, which has no record)
        let parts: Option<Vec<Option<FormRecord>>> = normalized
            .split('-')
            .map(|part| {
                if part.chars().all(|c| c.is_ascii_digit()) {
                    Some(None)
                } else if let Some(found) = dictionary.lookup_any_case(part) {
                    Some(Some(found.1))
                } else {
                    dictionary.speller.check_any_case(part).then_some(None)
                }
            })
            .collect();
        if let Some(parts) = parts {
            let last = parts.last().copied().flatten();
            word.part_records = Some(parts);
            resolve(&mut word, Status::Ok, normalized, last);
            return word;
        }
    }
    let misspelling = dictionary
        .forms
        .get(normalized.as_str())
        .or_else(|| dictionary.forms.get(lowercase_first(&normalized).as_str()))
        .copied();
    if let Some(bad) = misspelling {
        // The lemmas the misspelling belongs to; for an expression ("bekannt maachen"), the word in
        // it closest to the misspelling ("machen" -> "maachen").
        let lowercase_word: Vec<u16> = lowercase.encode_utf16().collect();
        let mut corrections: Vec<StaticStr> = Vec::new();
        for &i in &dictionary.entry_indices(Some(bad)) {
            let lemma = dictionary.entries[i].lemma;
            let correction = lemma
                .split(' ')
                .min_by_key(|part| {
                    let part: Vec<u16> = part.to_lowercase().encode_utf16().collect();
                    edit_distance(&lowercase_word, &part)
                })
                .unwrap_or(lemma);
            if !corrections.contains(&correction) {
                corrections.push(correction);
            }
        }
        word.corrections = corrections;
        resolve(&mut word, Status::Wrong, normalized, None);
        return word;
    }
    if let Some(reading) = read_number(&lowercase) {
        // Read as a number: right as written, or to fix (accents, millions written together)
        // unless the spellchecker.lu dictionary spells it this way.
        let exact = reading.spelling == lowercase;
        if exact || !dictionary.speller.check_any_case(&normalized) {
            word.number = Some(reading);
            let status = if exact {
                Status::Number
            } else {
                Status::NumberToFix
            };
            resolve(&mut word, status, normalized, None);
            return word;
        }
    }
    // The spellchecker.lu dictionary knows many correct words the LOD lacks. Its case rule is
    // Hunspell's: a capital first letter is always accepted, since adjectives and participles
    // become nouns with one ("de Verongléckte", "d'Vereenegte Staaten").
    if dictionary.speller.check_any_case(&normalized) {
        resolve(&mut word, Status::Speller, normalized, None);
        return word;
    }
    if let Some(compound) = dictionary.split_compound(&normalized) {
        word.compound = Some(compound);
        resolve(&mut word, Status::Compound, normalized, None);
        return word;
    }
    let status = if dictionary.attested.contains(normalized.as_str())
        || dictionary
            .attested
            .contains(lowercase_first(&normalized).as_str())
    {
        Status::Attested
    } else if capitalised && !sentence_start {
        Status::Name
    } else {
        Status::Unknown
    };
    resolve(&mut word, status, normalized, None);
    word
}

/// First letter of a word, lower case, ignoring a leading apostrophe
/// (`word.replace(/^'/, '').charAt(0).toLowerCase()` in the reference).
fn initial(word: &str) -> String {
    word.strip_prefix('\'')
        .unwrap_or(word)
        .chars()
        .next()
        .map(lowercase_char)
        .unwrap_or_default()
}

// ---------- remarks ----------
#[derive(Clone, Copy, PartialEq)]
enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    /// The name the page receives.
    fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "err",
            Severity::Warning => "warn",
            Severity::Info => "info",
        }
    }
    /// Per-word code in the output: 1 error, 2 warning, 3 info (0 = no remark).
    fn code(self) -> u8 {
        match self {
            Severity::Error => 1,
            Severity::Warning => 2,
            Severity::Info => 3,
        }
    }
}

/// Remark categories: stable keys the product maps to its own labels (and uses to decide, for
/// example, which fixes "fix all" applies).
mod category {
    pub const SPELLING: &str = "spelling";
    pub const COMPOUND: &str = "compound";
    pub const UNLISTED: &str = "unlisted";
    pub const PROPER_NOUN: &str = "proper_noun";
    pub const CAPITAL: &str = "capital";
    pub const N_RULE: &str = "n_rule";
    pub const PLURAL: &str = "plural";
    pub const GENDER: &str = "gender";
    pub const VERB: &str = "verb";
    pub const CONTRACTION: &str = "contraction";
    pub const TYPOGRAPHY: &str = "typography";
}

/// One remark about one word.
struct Issue {
    word_index: usize,
    severity: Severity,
    category: &'static str,
    /// What the remark is about, as a stable key ("n_drop", "gender_determiner"…). The engine
    /// writes no sentences: the product turns the rule and its parameters into its own message.
    rule: &'static str,
    /// Named values for the message: the words involved, the expected form…
    params: Vec<(&'static str, String)>,
    /// Replacements for the word, best first. An empty string means "delete the word".
    fixes: Vec<String>,
    /// UTF-16 offset where the fix ends when it covers more than the word itself ("an dem" → "am").
    end: Option<usize>,
}

impl Issue {
    fn new(
        word_index: usize,
        severity: Severity,
        category: &'static str,
        rule: &'static str,
        params: Vec<(&'static str, String)>,
        fixes: Vec<String>,
    ) -> Issue {
        Issue {
            word_index,
            severity,
            category,
            rule,
            params,
            fixes,
            end: None,
        }
    }
    fn ending_at(mut self, end: usize) -> Issue {
        self.end = Some(end);
        self
    }
}

/// Named parameters of a remark, from string slices.
fn params(pairs: &[(&'static str, &str)]) -> Vec<(&'static str, String)> {
    pairs
        .iter()
        .map(|&(name, value)| (name, value.to_string()))
        .collect()
}

/// Parameter names of the translations of a word, in the order of LANGUAGES.
const GLOSSES: [&str; 4] = ["gloss_fr", "gloss_en", "gloss_de", "gloss_pt"];
const FIRST_GLOSSES: [&str; 4] = ["first_fr", "first_en", "first_de", "first_pt"];
const SECOND_GLOSSES: [&str; 4] = ["second_fr", "second_en", "second_de", "second_pt"];

/// Adds the word's LOD translation in every language, under `names`.
fn add_glosses(
    dictionary: &Dictionary,
    values: &mut Vec<(&'static str, String)>,
    names: &[&'static str; 4],
    record: Option<FormRecord>,
) {
    for (language, name) in names.iter().enumerate() {
        values.push((name, dictionary.gloss(record, language).to_string()));
    }
}

/// params() followed by gloss_fr, gloss_en, gloss_de, gloss_pt.
fn glossed(
    dictionary: &Dictionary,
    pairs: &[(&'static str, &str)],
    record: Option<FormRecord>,
) -> Vec<(&'static str, String)> {
    let mut values = params(pairs);
    add_glosses(dictionary, &mut values, &GLOSSES, record);
    values
}

/// Removes the n-rule remark on a word, when another remark replaces the whole word anyway.
fn drop_n_rule(issues: &mut Vec<Issue>, word_index: usize) {
    issues.retain(|issue| !(issue.word_index == word_index && issue.category == category::N_RULE));
}

const NUMBER_WORDS: [&str; 13] = [
    "zwee", "zwou", "dräi", "véier", "fënnef", "sechs", "siwen", "aacht", "néng", "zéng", "eelef",
    "zwielef", "honnert",
];
/// Nouns that stay singular after a number (units, "Auer" for the time, "Mol"/"Kéier" for times).
const UNITS: [&str; 30] = [
    "Euro",
    "Cent",
    "Frang",
    "Dollar",
    "Kilo",
    "Gramm",
    "Meter",
    "Kilometer",
    "Zentimeter",
    "Millimeter",
    "Liter",
    "Prozent",
    "Grad",
    "Mol",
    "Kéier",
    "Auer",
    "Stéck",
    "Glas",
    "Taass",
    "Läffel",
    "Pond",
    "Fouss",
    "Zoll",
    "Dosen",
    "Pak",
    "Paar",
    "Puer",
    "Mann",
    "Joer",
    "Pabeier",
];
/// "an" is left out: it is also the conjunction "and" (tëschent X an dem Y).
const CONTRACTIONS: [(&str, &str); 4] =
    [("vun", "vum"), ("zu", "zum"), ("bei", "beim"), ("op", "um")];
const SUBJECT_PRONOUNS: [&str; 14] = [
    "ech", "du", "de", "hien", "hatt", "si", "se", "et", "mir", "mer", "dir", "der", "en", "s",
];
const POSSESSIVES: [&str; 6] = ["säi", "säin", "seng", "hir", "hiren", "hire"];
const ABBREVIATIONS: [&str; 14] = [
    "asw", "bzw", "etc", "ca", "resp", "vgl", "nr", "dr", "hr", "fr", "st", "z", "u", "mme",
];

// ---------- analysis ----------
/// Where a word stands in its sentence.
#[derive(Clone, Copy)]
struct SentenceStart {
    /// Start of the text, or after . ! ? … : or a line break. Capitals are expected here, so a
    /// capitalised common word is read as that word ("Den" is the article, not "Den Haag").
    loose: bool,
    /// Start of the text, or after a single . ! ? (not after … : "..." or a line break). Only here
    /// is a lower-case first letter flagged.
    strict: bool,
}

/// The text being analysed, as the rules see it.
struct Analysis<'a> {
    dictionary: &'a Dictionary,
    chars: &'a [char],
    /// UTF-16 offset of every char boundary: what the textarea and String.slice use.
    utf16_offsets: &'a [usize],
    words: &'a [Word],
    starts: &'a [SentenceStart],
}

impl<'a> Analysis<'a> {
    /// The characters between word `index` and the next word, or the end of the text.
    fn gap_after(&self, index: usize) -> &'a [char] {
        let end = self.words[index].end;
        match self.words.get(index + 1) {
            Some(next) => &self.chars[end..next.start],
            None => &self.chars[end..],
        }
    }
    /// A next word exists and only spaces or tabs separate it from word `index`.
    fn next_is_adjacent(&self, index: usize) -> bool {
        index + 1 < self.words.len() && is_blank(self.gap_after(index))
    }
    fn slice(&self, start: usize, end: usize) -> String {
        self.chars[start..end].iter().collect()
    }
    fn lowercase(&self, index: usize) -> String {
        self.words[index].normalized.to_lowercase()
    }
    /// Every LOD entry of the word has this part of speech (and there is at least one).
    fn only_part_of_speech(&self, word: &Word, part_of_speech: &str) -> bool {
        let entries = self.dictionary.entry_indices(word.record);
        !entries.is_empty()
            && entries
                .iter()
                .all(|&i| self.dictionary.entries[i].part_of_speech == part_of_speech)
    }
    /// A noun right after word `index`, or after one adjective or number in between.
    fn noun_after(&self, index: usize) -> Option<usize> {
        if !self.next_is_adjacent(index) {
            return None;
        }
        let mut candidate = index + 1;
        let next = &self.words[candidate];
        if !starts_upper(&next.text)
            && next.status == Status::Ok
            && self.next_is_adjacent(candidate)
        {
            let entries = self.dictionary.entry_indices(next.record);
            if !entries.is_empty()
                && entries
                    .iter()
                    .all(|&i| matches!(self.dictionary.entries[i].part_of_speech, "ADJ" | "NB"))
            {
                candidate += 1;
            }
        }
        let word = &self.words[candidate];
        (word.status == Status::Ok
            && !word.clitic
            && starts_upper(&word.text)
            && self.only_part_of_speech(word, "SUBST"))
        .then_some(candidate)
    }
    /// When `word` is a present-tense form of a verb, that verb's present form for `person`
    /// (0 ech … 5 si) and the verb's lemma.
    fn present_form(&self, word: &Word, person: usize) -> Option<(String, StaticStr)> {
        let dictionary = self.dictionary;
        let (forms, lemma) = dictionary
            .entry_indices(word.record)
            .iter()
            .find_map(|&i| {
                dictionary
                    .conjugations
                    .get(&(i as u32))
                    .map(|forms| (forms, dictionary.entries[i].lemma))
            })?;
        let form = word.form.as_str();
        let is_present = forms[2..8].iter().any(|cell| {
            cell.split(" / ").any(|alternative| {
                let verb = alternative.split(' ').next().unwrap_or("");
                verb == form || format!("{verb}n") == form || format!("{verb}nn") == form
            })
        });
        let wanted = first_form(forms[2 + person]).to_string();
        (is_present && !wanted.is_empty()).then_some((wanted, lemma))
    }
}

/// First form in a conjugation table cell: "spillen / spille" → "spillen", "ginn eran" → "ginn".
fn first_form(cell: &str) -> &str {
    cell.split(" / ")
        .next()
        .unwrap_or("")
        .split(' ')
        .next()
        .unwrap_or("")
}

/// Number of combining marks in a word (é, ë…), counted on its decomposed form.
fn mark_count(dictionary: &Dictionary, word: &str) -> usize {
    // Decomposed first: "é" is one char but e + a combining acute accent.
    let mut decomposed = Vec::new();
    for c in word.to_lowercase().chars() {
        dictionary.push_nfd(c, &mut decomposed);
    }
    decomposed
        .iter()
        .filter(|u| (0x300..=0x36f).contains(*u))
        .count()
}

/// Splits the text into words and records where each one stands in its sentence.
fn classify_words(
    dictionary: &Dictionary,
    chars: &[char],
    tokens: &[Token],
) -> (Vec<Word>, Vec<SentenceStart>) {
    let mut words = Vec::new();
    let mut starts = Vec::new();
    let mut loose = true;
    let mut strict = true;
    for token in tokens {
        match token.kind {
            TokenKind::Word => {
                words.push(classify(dictionary, chars, token, loose));
                starts.push(SentenceStart { loose, strict });
                loose = false;
                strict = false;
            }
            TokenKind::Space => {
                if chars[token.start..token.end].contains(&'\n') {
                    loose = true;
                }
            }
            TokenKind::Punctuation => {
                let c = chars[token.start];
                if matches!(c, '.' | '!' | '?' | '\u{2026}' | ':') {
                    loose = true;
                }
                if matches!(c, '.' | '!' | '?') {
                    // "..." written as three dots is not a full stop
                    let dots = (token.start > 0 && chars[token.start - 1] == '.')
                        || chars.get(token.end) == Some(&'.');
                    strict = !dots;
                } else if c == '\u{2026}' || c.is_alphabetic() || c.is_numeric() {
                    strict = false;
                }
                // A letter outside the word alphabet (Greek, Cyrillic…) is a punctuation token: it
                // ends a strict start but not a loose one. Other punctuation (quotes, brackets,
                // emoji, bullets…) leaves both states as they are.
            }
            TokenKind::Number => {
                loose = false;
                strict = false;
            }
        }
    }
    (words, starts)
}

// ---------- first pass: one word at a time ----------

/// Which dictionary knows a word: "lod", "spellchecker" (the spellchecker.lu dictionary only) or "".
fn source_of(dictionary: &Dictionary, word: &str) -> &'static str {
    if dictionary.lookup_any_case(word).is_some() {
        "lod"
    } else if dictionary.speller.check_any_case(word) {
        "spellchecker"
    } else {
        ""
    }
}

/// Adds the first correction and the dictionary it comes from ("close", "close_source"), so the
/// product can say who knows it.
fn with_closest(
    dictionary: &Dictionary,
    mut values: Vec<(&'static str, String)>,
    fixes: &[String],
) -> Vec<(&'static str, String)> {
    if let Some(first) = fixes.first() {
        values.push(("close", first.clone()));
        values.push(("close_source", source_of(dictionary, first).to_string()));
    }
    values
}

/// A correction with the same letters once accents are removed: "missing_accent" when the
/// correction has more accents ("drenken" -> "drénken"), "accent_variant" otherwise ("Déclinaisoun"
/// -> "Declinaisoun"). That correction moves first; the parameters say which dictionary spells it.
fn accent_remark(
    dictionary: &Dictionary,
    text: &str,
    fixes: &mut Vec<String>,
) -> Option<(&'static str, Vec<(&'static str, String)>)> {
    let bare = dictionary.no_marks(text);
    let marks = mark_count(dictionary, text);
    let position = fixes.iter().position(|fix| {
        dictionary.no_marks(fix) == bare
            && mark_count(dictionary, fix) != marks
            && starts_upper(fix) == starts_upper(text)
    })?;
    let expected = fixes.remove(position);
    fixes.insert(0, expected.clone());
    let rule = if mark_count(dictionary, &expected) > marks {
        "missing_accent"
    } else {
        "accent_variant"
    };
    let source = source_of(dictionary, &expected);
    Some((
        rule,
        params(&[("word", text), ("accented", &expected), ("source", source)]),
    ))
}

/// Unknown words, known misspellings, missing accents, compounds, capitals.
fn check_spelling(analysis: &Analysis, index: usize, issues: &mut Vec<Issue>) {
    let dictionary = analysis.dictionary;
    let word = &analysis.words[index];
    let text = word.text.as_str();
    let mut add = |severity, category, rule, params, fixes| {
        issues.push(Issue::new(index, severity, category, rule, params, fixes))
    };
    match word.status {
        Status::Ok | Status::Speller | Status::Number => {}
        Status::NumberToFix => {
            let Some(reading) = &word.number else {
                return;
            };
            let expected = keep_case(text, &reading.spelling);
            let value = reading.value.to_string();
            // Millions stand apart: a separate word, so a warning; otherwise the accents.
            let (severity, rule) = if reading.spelling.contains(' ') {
                (Severity::Warning, "number_split")
            } else {
                (Severity::Error, "number_spelling")
            };
            add(
                severity,
                category::SPELLING,
                rule,
                params(&[("word", text), ("expected", &expected), ("value", &value)]),
                vec![expected.clone()],
            );
        }
        Status::Wrong => add(
            Severity::Error,
            category::SPELLING,
            "known_misspelling",
            params(&[("word", text)]),
            word.corrections
                .iter()
                .map(|correction| keep_case(text, correction))
                .collect(),
        ),
        Status::Unknown => {
            let suggestions = dictionary.suggest_spelling(&word.normalized, 4);
            let mut fixes: Vec<String> = suggestions
                .iter()
                .map(|suggestion| keep_case(text, suggestion))
                .collect();
            if let Some((rule, values)) = accent_remark(dictionary, text, &mut fixes) {
                add(Severity::Error, category::SPELLING, rule, values, fixes);
            } else if fixes.is_empty() && starts_upper(text) {
                // Capitalised at the start of a sentence, unknown, and close to nothing: as likely a
                // name ("Amélie, Célia & Arby") as a mistake, so an info like mid-sentence.
                add(
                    Severity::Info,
                    category::PROPER_NOUN,
                    "proper_noun",
                    params(&[("word", text)]),
                    fixes,
                );
            } else {
                let values = with_closest(dictionary, params(&[("word", text)]), &fixes);
                add(
                    Severity::Error,
                    category::SPELLING,
                    "unknown_word",
                    values,
                    fixes,
                );
            }
        }
        Status::Compound => {
            let Some((first, first_record, second, second_record)) = &word.compound else {
                return;
            };
            // Noun + noun is how Luxembourgish makes new words ("Medezinstudent",
            // "Gewaltzeenen"): no remark. Other splits ("Laf" + "technik", "dohi" + "gefall")
            // are worth a look.
            let only_nouns = |record: &FormRecord| {
                let entries = dictionary.entry_indices(Some(*record));
                !entries.is_empty()
                    && entries
                        .iter()
                        .all(|&i| dictionary.entries[i].part_of_speech == "SUBST")
            };
            // ... unless a known word is one typing slip away ("Bandwonn" for "Brandwonn",
            // "Gebuertdag" for "Gebuertsdag"): then it is more likely a typo.
            let fixes = dictionary.suggest_spelling(&word.normalized, 3);
            let near = fixes
                .iter()
                .any(|fix| one_edit_apart(&word.normalized, fix));
            if starts_upper(text) && only_nouns(first_record) && only_nouns(second_record) && !near
            {
                return;
            }
            let mut values = params(&[("word", text), ("first", first), ("second", second)]);
            add_glosses(dictionary, &mut values, &FIRST_GLOSSES, Some(*first_record));
            add_glosses(
                dictionary,
                &mut values,
                &SECOND_GLOSSES,
                Some(*second_record),
            );
            add(
                Severity::Warning,
                category::COMPOUND,
                "compound",
                values,
                fixes,
            );
        }
        Status::Attested => add(
            Severity::Info,
            category::UNLISTED,
            "unlisted_attested",
            params(&[("word", text)]),
            Vec::new(),
        ),
        Status::Name => {
            // Close LOD names decide whether this is a mistake; the corrections offered come from
            // both dictionaries, ranked together.
            let names: Vec<StaticStr> = dictionary
                .suggest(&word.normalized, 4)
                .into_iter()
                .filter(|suggestion| starts_upper(suggestion))
                .collect();
            let mut fixes: Vec<String> = dictionary
                .suggest_spelling(&word.normalized, 6)
                .into_iter()
                .filter(|suggestion| starts_upper(suggestion))
                .take(4)
                .collect();
            if fixes.is_empty() {
                fixes = names.iter().map(|name| name.to_string()).collect();
            }
            if let Some((rule, values)) = accent_remark(dictionary, text, &mut fixes) {
                add(Severity::Error, category::SPELLING, rule, values, fixes);
            } else if !names.is_empty() {
                let values = with_closest(dictionary, params(&[("word", text)]), &fixes);
                add(
                    Severity::Error,
                    category::SPELLING,
                    "unknown_capitalised",
                    values,
                    fixes,
                );
            } else {
                // Maybe a name, maybe a typo of a word only the spellchecker.lu dictionary has:
                // stay an info, but offer its corrections and say where the closest one comes from.
                let values = with_closest(dictionary, params(&[("word", text)]), &fixes);
                add(
                    Severity::Info,
                    category::PROPER_NOUN,
                    "proper_noun",
                    values,
                    fixes,
                );
            }
        }
        Status::Case => {
            let form = word.form.as_str();
            if starts_upper(text) && used_as_noun(analysis, index) {
                // "eisen Eelsten", "d'Faaschten", "am Zweete Weltkrich": the capital is right.
            } else if starts_upper(text) {
                // A capital the LOD does not have: the lower-case word, then capitalised neighbours.
                let mut fixes = vec![form.to_string()];
                fixes.extend(
                    dictionary
                        .suggest_spelling(&word.normalized, 8)
                        .into_iter()
                        .filter(|suggestion| starts_upper(suggestion) && suggestion != form)
                        .take(3),
                );
                add(
                    Severity::Warning,
                    category::CAPITAL,
                    "capital_unexpected",
                    glossed(dictionary, &[("word", text), ("lower", form)], word.record),
                    fixes,
                );
            } else {
                // A noun written in lower case.
                add(
                    Severity::Warning,
                    category::CAPITAL,
                    "capital_missing",
                    glossed(dictionary, &[("noun", form)], word.record),
                    vec![form.to_string()],
                );
            }
        }
    }
}

/// Words that take a determiner: after one of them, a capitalised adjective, infinitive or
/// number is used as a noun ("eisen Eelsten", "d'Faaschten", "beim Schwammen", "eppes Neies").
const NOUN_MAKERS: [&str; 62] = [
    "d'", "d’", "de", "den", "der", "dat", "déi", "dem", "deen", "dee", "deem", "deenen", "e",
    "en", "eng", "engem", "enger", "engen", "ee", "een", "eise", "eist", "eisen", "eiser", "eisem",
    "mäi", "mäin", "meng", "mengem", "menger", "mengen", "däi", "däin", "deng", "säi", "säin",
    "seng", "sengem", "senger", "sengen", "hir", "hire", "hiren", "är", "äre", "ären", "beim",
    "am", "vum", "zum", "um", "nom", "virum", "ville", "vill", "puer", "all", "eppes", "näischt",
    "keen", "kee", "keng",
];

/// The lower-case reading of a capitalised word is an infinitive ("Spullen", "Schwammen") or an
/// inflected adjective ("Franséischen", "Alen"): written with a capital, it is used as a noun.
fn lower_reads_as_noun(dictionary: &Dictionary, form: &str) -> bool {
    let lower = lowercase_first(form);
    let Some(record) = dictionary.valid_record(&lower) else {
        return false;
    };
    dictionary.entry_indices(Some(record)).iter().any(|&i| {
        let entry = &dictionary.entries[i];
        match entry.part_of_speech {
            "ADJ" => entry.lemma != lower,
            "VRB" => {
                entry.lemma == lower
                    || dictionary.n_dropped.get(entry.lemma) == Some(&lower.as_str())
            }
            _ => false,
        }
    })
}

/// Prepositions after which a capitalised adjective or infinitive is a noun: "vu Klengem un",
/// "zënter Kuerzem", "mat Programméieren", "als Éischten".
const NOUN_PREPOSITIONS: [&str; 22] = [
    "vu", "vun", "zënter", "zanter", "viru", "virun", "mat", "bei", "als", "u", "un", "no", "ouni",
    "fir", "iwwer", "iwwerem", "ënner", "mam", "duerch", "géint", "deene", "där",
];

/// A word that can come before the noun in a noun phrase: a determiner, a preposition with or
/// without its article, a number, "meescht", or a lower-case inflected adjective ("no
/// stonnelaangem Waarden").
fn noun_phrase_word(analysis: &Analysis, index: usize) -> bool {
    let lower = analysis.lowercase(index);
    let lower = lower.as_str();
    if NOUN_MAKERS.contains(&lower)
        || NOUN_PREPOSITIONS.contains(&lower)
        || NUMBER_WORDS.contains(&lower)
        || matches!(
            lower,
            "meescht" | "des" | "dës" | "dësem" | "dëser" | "dësen" | "dëse"
        )
    {
        return true;
    }
    let word = &analysis.words[index];
    !starts_upper(&word.text)
        && analysis.only_part_of_speech(word, "ADJ")
        && ["e", "en", "em", "er", "es"]
            .iter()
            .any(|ending| lower.ends_with(ending))
}

/// A capitalised noun of the LOD one typing slip away (a letter missing, added, replaced or two
/// swapped): "Kache" for "Knache", "Aler" for "Alter" are more likely typos than nouns made from
/// "kachen" or "al".
fn near_a_noun(dictionary: &Dictionary, word: &str) -> bool {
    dictionary
        .suggest_spelling(word, 6)
        .iter()
        .any(|candidate| {
            starts_upper(candidate)
                && one_edit_apart(word, candidate)
                && dictionary.valid_record(candidate).is_some_and(|record| {
                    dictionary
                        .entry_indices(Some(record))
                        .iter()
                        .all(|&i| dictionary.entries[i].part_of_speech == "SUBST")
                })
        })
}

fn one_edit_apart(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a == b || a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let start = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let end = a[start..]
        .iter()
        .rev()
        .zip(b[start..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (rest_a, rest_b) = (a.len() - start - end, b.len() - start - end);
    // one letter replaced, added or missing, or two neighbours swapped
    rest_a.max(rest_b) <= 1
        || (rest_a == 2 && rest_b == 2 && a[start] == b[start + 1] && a[start + 1] == b[start])
}

/// The form is the entry's past participle with an adjective ending: "Beschëllegten",
/// "Blesséierten" (used as nouns).
fn inflected_participle(dictionary: &Dictionary, entry: usize, form: &str) -> bool {
    let Some(table) = dictionary.conjugations.get(&(entry as u32)) else {
        return false;
    };
    table[1].split(" / ").any(|participle| {
        form.strip_prefix(participle)
            .is_some_and(|ending| matches!(ending, "e" | "en" | "em" | "er" | "es"))
    })
}

/// A capitalised word the LOD only has in lower case, used as a noun: after a determiner or a
/// preposition with its article ("eisen Eelsten", "d'Faaschten", "beim Schwammen"), or as the
/// first part of a name before a capitalised noun ("Zweete Weltkrich", "Gréng Zeiselen").
fn used_as_noun(analysis: &Analysis, index: usize) -> bool {
    let words = analysis.words;
    // Only an adjective, a number or a verb's infinitive becomes a noun: "d'Baut" is a typo of
    // "d'Braut", not the verb form "baut" as a noun.
    let dictionary = analysis.dictionary;
    let form = words[index].form.as_str();
    let nominal = dictionary
        .entry_indices(words[index].record)
        .iter()
        .any(|&i| {
            let entry = &dictionary.entries[i];
            match entry.part_of_speech {
                // an adjective used as a noun takes an ending ("Alen", "Eelsten"; "Séier" alone is
                // more likely a typo of "Stéier")
                "ADJ" => entry.lemma != form,
                "NB" => true,
                "VRB" => {
                    entry.lemma == form
                        || dictionary.n_dropped.get(entry.lemma) == Some(&form)
                        || inflected_participle(dictionary, i, form)
                }
                _ => false,
            }
        });
    if !nominal {
        return false;
    }
    let joined = index > 0 && (is_blank(analysis.gap_after(index - 1)) || words[index - 1].clitic);
    // After a determiner the noun reading is clear. After a preposition or an adjective it is
    // weaker: a LOD noun one typing slip away wins ("Kache" for "Knache", "Aler" for "Alter").
    if joined && NOUN_MAKERS.contains(&analysis.lowercase(index - 1).as_str()) {
        return true;
    }
    if joined
        && noun_phrase_word(analysis, index - 1)
        && !near_a_noun(dictionary, &words[index].normalized)
    {
        return true;
    }
    analysis.next_is_adjacent(index)
        && starts_upper(&words[index + 1].text)
        && analysis.only_part_of_speech(&words[index + 1], "SUBST")
}

/// The n-rule (Eifeler Regel): a final n stays before a vowel and n d t z h, and at the end of a
/// sentence; it drops before other consonants.
fn check_n_rule(analysis: &Analysis, index: usize, issues: &mut Vec<Issue>) {
    let dictionary = analysis.dictionary;
    let word = &analysis.words[index];
    let text = word.text.as_str();
    let next = analysis.words.get(index + 1);
    let next_initial = next
        .map(|next| initial(&next.normalized))
        .unwrap_or_default();
    let adjacent = analysis.next_is_adjacent(index);
    let after_s = index > 0 && analysis.words[index - 1].normalized.ends_with('s');

    // At the start of a sentence the capital is the sentence's: "E nervösen Houscht" is the
    // article "e", not the letter "E" (the LOD entry of the capitalised form).
    let mut record = word.record;
    if analysis.starts[index].loose
        && record.is_none_or(|record| record.full_forms.is_empty())
        && text.chars().next().is_some_and(char::is_uppercase)
    {
        // Only a word of its own: "nu" is the n-less form only inside "nu mol" / "nun emol".
        if let Some(lower) = dictionary.valid_record(&lowercase_first(&word.form)) {
            let single_word = dictionary
                .entry_indices(Some(lower))
                .iter()
                .any(|&i| !dictionary.entries[i].lemma.contains(' '));
            if !lower.full_forms.is_empty() && single_word {
                record = Some(lower);
            }
        }
    }
    let mut full_forms: Vec<&str> = match record {
        Some(record) if !word.clitic && !record.full_forms.is_empty() => {
            record.full_forms.split('|').collect()
        }
        _ => Vec::new(),
    };
    let mut only_n_variant = record.is_some_and(|record| record.only_n_variant);
    if !word.clitic {
        if let Some(full) = forced_full_form(&word.form) {
            full_forms = vec![full];
            // "des de" is the genitive "de", not "den" without its n
            only_n_variant = !(word.form == "de" && after_s);
        }
    }

    if only_n_variant && !full_forms.is_empty() {
        // The word is a form without its n: is the n needed here?
        if ech_du_without_form(analysis, index, full_forms[0]) {
            return;
        }
        let full = keep_case(text, full_forms[0]);
        match next {
            Some(next)
                if adjacent && keeps_n_before(dictionary, next, &next_initial) == Some(true) =>
            {
                // What keeps the n: one of n d t z h, or a vowel.
                // ("sound": the letter does not keep the n, the LOD's pronunciation does: Job, Chip)
                let before = if matches!(next_initial.as_str(), "n" | "d" | "t" | "z" | "h") {
                    next_initial.as_str()
                } else if in_set("aeiouäëéèêöüàâîôû", &next_initial) {
                    "vowel"
                } else {
                    "sound"
                };
                issues.push(Issue::new(
                    index,
                    Severity::Error,
                    category::N_RULE,
                    "n_keep",
                    params(&[("next", &next.text), ("before", before), ("full", &full)]),
                    vec![full],
                ));
            }
            _ if sentence_end(analysis.gap_after(index)) => issues.push(Issue::new(
                index,
                Severity::Error,
                category::N_RULE,
                "n_end",
                params(&[("full", &full)]),
                vec![full],
            )),
            _ => {}
        }
        return;
    }

    // The word has its n: should it drop?
    let Some(next) = next.filter(|_| !word.clitic && adjacent) else {
        return;
    };
    if keeps_n_before(dictionary, next, &next_initial) != Some(false)
        || is_s_function_word(&next.normalized.to_lowercase())
    {
        return;
    }
    // A Y that drops the n is the consonant [j]: the "sound" parameter says so, since the letter
    // alone looks like a vowel.
    let sound = if next_initial == "y" { "j" } else { "" };
    // At the start of a sentence the capital is the sentence's, not the word's: "Wann mir" is
    // "wann mir" ("Hunn", the rooster, keeps its own entry when it has one).
    let lowered;
    let mut form = word.form.as_str();
    if !dictionary.n_dropped.contains_key(form) && analysis.starts[index].loose {
        lowered = lowercase_first(form);
        form = lowered.as_str();
    }
    if let Some(&without_n) = dictionary.n_dropped.get(form) {
        if dictionary.valid_record(without_n).is_some() && form != without_n {
            let fixed = keep_case(text, without_n);
            issues.push(Issue::new(
                index,
                Severity::Warning,
                category::N_RULE,
                "n_drop",
                params(&[
                    ("next", &next.text),
                    ("initial", &next_initial),
                    ("sound", sound),
                    ("fixed", &fixed),
                ]),
                vec![fixed],
            ));
        }
    }
}

/// "ee" and "een" are also the pronoun "one" ("datt ee Musek…", "Fir dat brauch een
/// Erfarung"): only read them as an article after a preposition, or after a verb that has its
/// own subject pronoun just before it.
fn is_een_pronoun(analysis: &Analysis, index: usize) -> bool {
    if !matches!(analysis.lowercase(index).as_str(), "ee" | "een") {
        return false;
    }
    if index == 0 {
        return true;
    }
    let previous = &analysis.words[index - 1];
    let subject_before = index >= 2
        && matches!(
            analysis.lowercase(index - 2).as_str(),
            "ech" | "du" | "hien" | "hatt" | "si" | "mir" | "dir" | "mer" | "der" | "de"
        );
    let after_conjunction = matches!(
        previous.normalized.to_lowercase().as_str(),
        "datt"
            | "dass"
            | "wann"
            | "wa"
            | "well"
            | "ob"
            | "wéi"
            | "andeem"
            | "andeems"
            | "bis"
            | "soubal"
            | "obwuel"
    );
    after_conjunction || (analysis.only_part_of_speech(previous, "VRB") && !subject_before)
}

/// Determiner (or "zwee" / "zwou") and the gender or number of the noun after it, with at most
/// one adjective in between.
fn check_determiner(analysis: &Analysis, index: usize, issues: &mut Vec<Issue>) {
    let dictionary = analysis.dictionary;
    let words = analysis.words;
    let word = &words[index];
    let text = word.text.as_str();
    let lowercase = analysis.lowercase(index);
    let determiner = dictionary.determiners.get(lowercase.as_str());
    // "zwee" and "zwou" agree in gender too: zwee is masculine or neuter, zwou feminine.
    let number_genders: Option<&'static str> = match lowercase.as_str() {
        "zwee" => Some("MN"),
        "zwou" => Some("F"),
        _ => None,
    };
    let accepted_genders = match (number_genders, determiner) {
        (Some(genders), _) => genders,
        (None, Some(determiner)) => determiner.genders,
        (None, None) => return,
    };
    // "allen zwee" means both: no agreement with a noun after it
    if number_genders.is_some()
        && index > 0
        && matches!(
            analysis.lowercase(index - 1).as_str(),
            "allen" | "all" | "alle" | "allebéid"
        )
    {
        return;
    }
    let after_s = index > 0 && words[index - 1].normalized.ends_with('s');
    if !analysis.next_is_adjacent(index)
        || (lowercase == "de" && after_s)
        || is_een_pronoun(analysis, index)
    {
        return;
    }
    let next = &words[index + 1];
    let next_initial = initial(&next.normalized);
    let variant_keeps_n =
        next_initial.is_empty() || keeps_n_before(dictionary, next, &next_initial) == Some(true);

    let mut noun = next;
    let mut noun_index = index + 1;
    // A number in between ("um dräi Auer", "den dräi Kanner") changes the agreement: no check then.
    let mut counted = false;
    if let Some(after) = words.get(index + 2) {
        if !starts_upper(&noun.text)
            && noun.status == Status::Ok
            && is_blank(&analysis.chars[noun.end..after.start])
        {
            let entries = dictionary.entry_indices(noun.record);
            let part_of_speech = |i: usize| dictionary.entries[i].part_of_speech;
            if !entries.is_empty()
                && entries
                    .iter()
                    .all(|&i| matches!(part_of_speech(i), "ADJ" | "NB"))
            {
                counted = entries.iter().all(|&i| part_of_speech(i) == "NB");
                noun = after;
                noun_index = index + 2;
            }
        }
    }
    if noun.status != Status::Ok || noun.clitic || !starts_upper(&noun.text) || counted {
        return;
    }
    // "zwou Slow Motions": another capitalised word right after, the noun is a multi-word name
    if number_genders.is_some()
        && analysis.next_is_adjacent(noun_index)
        && starts_upper(&words[noun_index + 1].text)
    {
        return;
    }

    let entries = dictionary.entry_indices(noun.record);
    let noun_form = noun.form.as_str();
    let is_n_form_of = |lemma: &str| {
        lemma
            .strip_prefix(noun_form)
            .is_some_and(|rest| rest == "n" || rest == "nn")
    };
    let is_plural = !entries.is_empty()
        && entries.iter().all(|&i| {
            let entry = &dictionary.entries[i];
            if entry.part_of_speech != "SUBST" {
                return false;
            }
            // plural-only nouns (Leit, Elteren, Käschten…) have no gender in the LOD
            if entry.lemma == noun_form {
                return entry.gender.is_empty()
                    && (noun_form.ends_with("en")
                        || noun_form.ends_with("leit")
                        || noun_form.ends_with("Leit")
                        || noun_form.ends_with("eger"));
            }
            !is_n_form_of(entry.lemma)
        });
    let span = analysis.slice(next.start, noun.end);
    // "beim Spullen", "am Franséischen": an infinitive or an adjective used as a noun, not a plural.
    let is_plural = is_plural && !lower_reads_as_noun(dictionary, noun_form);

    // A plural noun after a singular determiner that has a plural form.
    if let (Some(plural), true, Some(&first)) = (
        determiner.and_then(|d| d.plural.as_deref()),
        is_plural,
        entries.first(),
    ) {
        let plural_determiner = keep_case(text, pick_variant(plural, variant_keeps_n));
        let lemma = dictionary.entries[first].lemma;
        // The lemma only when it differs from the noun as written ("Kanner", of "Kand").
        let of_lemma = if lemma != noun_form { lemma } else { "" };
        drop_n_rule(issues, index);
        issues.push(Issue::new(
            index,
            Severity::Warning,
            category::PLURAL,
            "plural_determiner",
            params(&[
                ("noun", &noun.text),
                ("lemma", of_lemma),
                ("fixed", &format!("{plural_determiner} {span}")),
                ("current", &format!("{text} {span}")),
            ]),
            vec![plural_determiner],
        ));
        return;
    }

    // The genders of the noun, when every reading of it agrees.
    let mut genders = String::new();
    let mut sure = !entries.is_empty();
    for &i in &entries {
        let entry = &dictionary.entries[i];
        let is_lemma = entry.lemma == noun_form;
        let is_plural_form =
            !entry.plural.is_empty() && entry.plural.split(", ").any(|p| p == noun_form);
        if entry.part_of_speech != "SUBST" || entry.gender.is_empty() {
            sure = false;
            continue;
        }
        let fits = if number_genders.is_some() {
            // after zwee / zwou: the plural, or a singular that has a plural
            // (check_number_plural flags it; the gender is the same)
            !is_n_form_of(entry.lemma) && (!is_lemma || is_plural_form || !entry.plural.is_empty())
        } else {
            // after an article the noun must be its singular lemma
            is_lemma && !is_plural_form
        };
        if fits {
            genders.push_str(entry.gender);
        } else {
            sure = false;
        }
    }
    if !sure
        || genders
            .chars()
            .any(|gender| accepted_genders.contains(gender))
    {
        return;
    }
    let Some(gender) = genders.chars().next() else {
        return;
    };
    let gender_code = match gender {
        'M' => "M",
        'F' => "F",
        _ => "N",
    };
    let replacement: Option<&str> = match number_genders {
        Some(_) => Some(if gender == 'F' { "zwou" } else { "zwee" }),
        None => determiner
            .and_then(|d| d.by_gender[gender_index(gender)].as_deref())
            .map(|variants| pick_variant(variants, variant_keeps_n)),
    };
    let Some(replacement) = replacement else {
        return;
    };
    let replacement = keep_case(text, replacement);
    let with_noun = |determiner: &str| {
        let space = if determiner.ends_with(is_apostrophe) {
            ""
        } else {
            " "
        };
        format!("{determiner}{space}{span}")
    };
    drop_n_rule(issues, index);
    issues.push(Issue::new(
        index,
        Severity::Warning,
        category::GENDER,
        "gender_determiner",
        params(&[
            ("noun", &noun.text),
            ("gender", gender_code),
            ("fixed", &with_noun(&replacement)),
            ("current", &with_noun(text)),
        ]),
        vec![replacement],
    ));
}

/// "ech" / "du" right before a verb whose form with its n is not that person's form:
/// "ech brauche" is not "ech brauchen" (the LOD conjugates "ech brauch"), so the n rule
/// has nothing to restore.
fn ech_du_without_form(analysis: &Analysis, index: usize, full: &str) -> bool {
    if index == 0 || !analysis.next_is_adjacent(index - 1) {
        return false;
    }
    let person = match analysis.lowercase(index - 1).as_str() {
        "ech" => 0,
        "du" => 1,
        _ => return false,
    };
    let word = &analysis.words[index];
    if !analysis.only_part_of_speech(word, "VRB") {
        return false;
    }
    let dictionary = analysis.dictionary;
    let cells: Vec<StaticStr> = dictionary
        .entry_indices(word.record)
        .iter()
        .filter_map(|&i| dictionary.conjugations.get(&(i as u32)))
        .map(|forms| forms[2 + person])
        .collect();
    // Every verb the word may be conjugates this person without the full form.
    !cells.is_empty()
        && cells.iter().all(|cell| {
            !cell
                .split(" / ")
                .any(|alternative| alternative.split(' ').next() == Some(full))
        })
}

/// A verb form proposed as a fix, with the n-rule applied to it: "Ech hutt gëschter" becomes
/// "Ech hu gëschter" in one step, not "hunn" and then a second remark for the n.
fn fit_n(analysis: &Analysis, verb_index: usize, wanted: &str) -> String {
    let dictionary = analysis.dictionary;
    if analysis.next_is_adjacent(verb_index) {
        let next = &analysis.words[verb_index + 1];
        // Same exceptions as the n-rule: before "si", "se", "seng…" the LOD keeps the n too.
        if keeps_n_before(dictionary, next, &initial(&next.normalized)) == Some(false)
            && !is_s_function_word(&next.normalized.to_lowercase())
        {
            if let Some(&short) = dictionary.n_dropped.get(wanted) {
                if dictionary.valid_record(short).is_some() {
                    return short.to_string();
                }
            }
        }
    }
    wanted.to_string()
}

/// Every fix, whatever the rule, gets the n-rule for the word after it: "zwou Stonn vun" →
/// "Stonne" (not "Stonnen", then a second remark), "drenken Kaffi" → "drénke". One click per
/// mistake. The n-rule's own remarks are already right.
fn fit_fixes_to_n_rule(analysis: &Analysis, issues: &mut [Issue]) {
    for issue in issues.iter_mut() {
        if issue.category == category::N_RULE {
            continue;
        }
        // The last word the fix replaces.
        let last = match issue.end {
            Some(end) => (issue.word_index..analysis.words.len())
                .find(|&j| analysis.utf16_offsets[analysis.words[j].end] == end),
            None => Some(issue.word_index),
        };
        let Some(last) = last else { continue };
        for fix in issue.fixes.iter_mut() {
            let (head, tail) = match fix.rfind(' ') {
                Some(space) => fix.split_at(space + 1),
                None => ("", fix.as_str()),
            };
            if tail.is_empty() {
                continue;
            }
            let fitted = fit_n(analysis, last, tail);
            if fitted != tail {
                *fix = format!("{head}{fitted}");
            }
        }
    }
}

/// The persons a subject pronoun stands for, as a mask (bit 0 ech … bit 5 si).
fn pronoun_persons(word: &str) -> u32 {
    match word {
        "ech" => 1,
        "du" | "de" | "s" => 1 << 1,
        "hien" | "hie" | "hatt" | "et" | "en" => 1 << 2,
        "si" | "se" => (1 << 2) | (1 << 5),
        "mir" | "mer" => 1 << 3,
        "dir" | "der" => 1 << 4,
        _ => 0,
    }
}

/// "ech" / "du" followed by a verb form of another person: "ech spillt" → "ech spillen".
fn check_ech_du_verb(analysis: &Analysis, index: usize, issues: &mut Vec<Issue>) {
    let dictionary = analysis.dictionary;
    let lowercase = analysis.lowercase(index);
    let person = match lowercase.as_str() {
        "ech" => 0,
        "du" => 1,
        _ => return,
    };
    if !analysis.next_is_adjacent(index) {
        return;
    }
    let verb = &analysis.words[index + 1];
    if starts_upper(&verb.text) || verb.status != Status::Ok {
        return;
    }
    let Some(&(persons, wild)) = dictionary.person_masks.get(verb.form.as_str()) else {
        return;
    };
    // A form that may be an infinitive or a participle ("wild") can follow "ech" / "du" after an
    // inverted verb ("Wëlls du kommen?"); at the start of a sentence the next word is the finite
    // verb: "Ech wësse net" → "Ech weess net".
    let wild = wild && !analysis.starts[index].loose;
    if wild || persons & (1 << person) != 0 || !analysis.only_part_of_speech(verb, "VRB") {
        return;
    }
    let conjugation = dictionary.entry_indices(verb.record).iter().find_map(|&i| {
        dictionary
            .conjugations
            .get(&(i as u32))
            .map(|forms| (forms, dictionary.entries[i].lemma))
    });
    let fix = match conjugation {
        Some((forms, lemma)) => {
            let wanted = first_form(forms[2 + person]);
            (!wanted.is_empty()).then(|| (fit_n(analysis, index + 1, wanted), lemma))
        }
        None => None,
    };
    let ((rule, values), fixes) = match fix {
        Some((wanted, lemma)) => (
            (
                "verb_form",
                params(&[
                    ("pronoun", &lowercase),
                    ("verb", &verb.text),
                    ("lemma", lemma),
                    ("expected", &format!("{lowercase} {wanted}")),
                ]),
            ),
            vec![wanted],
        ),
        None => (
            (
                "verb_mismatch",
                params(&[("pronoun", &lowercase), ("verb", &verb.text)]),
            ),
            Vec::new(),
        ),
    };
    issues.push(Issue::new(
        index + 1,
        Severity::Error,
        category::VERB,
        rule,
        values,
        fixes,
    ));
}

// ---------- second pass (Nov 2026): agreement, numbers, contractions, ze, typography ----------
// Each rule was measured on the LOD example sentences (the test corpus) to keep false alarms rare.

/// A subject pronoun at the start of a sentence, then a finite verb in the wrong person:
/// "Hien spillen" → "Hie spillt".
fn check_pronoun_verb(analysis: &Analysis, index: usize, issues: &mut Vec<Issue>) {
    let dictionary = analysis.dictionary;
    let words = analysis.words;
    let lowercase = analysis.lowercase(index);
    // (persons the verb may have, persons to suggest, pronoun may be a dative)
    let (allowed, targets, may_be_dative): (u32, &[usize], bool) = match lowercase.as_str() {
        "hien" | "hie" | "hatt" => (1 << 2, &[2], false),
        "si" => ((1 << 2) | (1 << 5), &[2, 5], false),
        "et" => ((1 << 2) | (1 << 5), &[2], false),
        "mir" => (1 << 3, &[3], true),
        "dir" => (1 << 4, &[4], true),
        _ => return,
    };
    if !analysis.starts[index].loose || !analysis.next_is_adjacent(index) {
        return;
    }
    let verb = &words[index + 1];
    if starts_upper(&verb.text)
        || verb.status != Status::Ok
        || !analysis.only_part_of_speech(verb, "VRB")
    {
        return;
    }
    // The "wild" flag (infinitive, participle…) is ignored on purpose: right after a subject
    // pronoun at the start of a sentence, the second word is the finite verb.
    let Some(&(persons, _wild)) = dictionary.person_masks.get(verb.form.as_str()) else {
        return;
    };
    // mir / dir are often datives ("Mir ass kal", "Dir ginn d'Aen op"): 3rd-person verbs are fine.
    let dative = may_be_dative && persons & ((1 << 2) | (1 << 5)) != 0;
    // "Mir si mer…": a subject pronoun after the verb means the first word is not the subject.
    // Only when the verb agrees with that second pronoun: "Hien hutt mer…" is still wrong.
    let inverted = analysis.next_is_adjacent(index + 1)
        && SUBJECT_PRONOUNS.contains(&analysis.lowercase(index + 2).as_str())
        && pronoun_persons(&analysis.lowercase(index + 2)) & persons != 0;
    if dative || inverted || persons & allowed != 0 {
        return;
    }
    let forms: Vec<(String, StaticStr)> = targets
        .iter()
        .filter_map(|&person| analysis.present_form(verb, person))
        .map(|(wanted, lemma)| (fit_n(analysis, index + 1, &wanted), lemma))
        .collect();
    let (rule, values) = match forms.first() {
        Some((wanted, lemma)) => (
            "verb_form",
            params(&[
                ("pronoun", &lowercase),
                ("verb", &verb.text),
                ("lemma", lemma),
                ("expected", &format!("{lowercase} {wanted}")),
            ]),
        ),
        None => (
            "verb_mismatch",
            params(&[("pronoun", &lowercase), ("verb", &verb.text)]),
        ),
    };
    let mut fixes: Vec<String> = forms.into_iter().map(|(wanted, _)| wanted).collect();
    fixes.dedup();
    // the verb is wrong anyway: an n-rule remark on it would fix the wrong form
    drop_n_rule(issues, index + 1);
    issues.push(Issue::new(
        index + 1,
        Severity::Error,
        category::VERB,
        rule,
        values,
        fixes,
    ));
}

/// A number written as a word, then a singular noun: "zwee Auto" → "zwee Autoen".
fn check_number_word(analysis: &Analysis, index: usize, issues: &mut Vec<Issue>) {
    let lowercase = analysis.lowercase(index);
    if !NUMBER_WORDS.contains(&lowercase.as_str()) {
        return;
    }
    // "allen zwee" means both; "d'aacht Joerhonnert" is an ordinal after an article. After the
    // plural "déi" the number counts: "déi zwee Hënn".
    let after_article_or_all = index > 0
        && matches!(
            analysis.lowercase(index - 1).as_str(),
            "allen"
                | "all"
                | "alle"
                | "allebéid"
                | "d'"
                | "den"
                | "de"
                | "dat"
                | "dem"
                | "der"
                | "am"
                | "um"
                | "vum"
                | "zum"
                | "beim"
        );
    if after_article_or_all {
        return;
    }
    if let Some(noun_index) = analysis.noun_after(index) {
        let before = issues.len();
        check_number_plural(analysis, noun_index, &lowercase, issues);
        if issues.len() > before && noun_index == index + 1 {
            merge_number_gender(analysis, index, noun_index, issues);
        }
    }
}

/// "zwee Kaz": zwee has the wrong gender and Kaz is singular. The two remarks become one, over
/// both words, with one fix: "zwou Kazen".
fn merge_number_gender(
    analysis: &Analysis,
    index: usize,
    noun_index: usize,
    issues: &mut Vec<Issue>,
) {
    let Some(gender_at) = issues
        .iter()
        .position(|issue| issue.word_index == index && issue.rule == "gender_determiner")
    else {
        return;
    };
    let Some(plural) = issues.last().and_then(|issue| issue.fixes.first().cloned()) else {
        return;
    };
    let Some(number) = issues[gender_at].fixes.first().cloned() else {
        return;
    };
    let gender = issues[gender_at]
        .params
        .iter()
        .find(|(name, _)| *name == "gender")
        .map(|(_, value)| value.clone())
        .unwrap_or_default();
    let words = analysis.words;
    let fixed = format!("{number} {plural}");
    let current = analysis.slice(words[index].start, words[noun_index].end);
    issues.pop();
    issues.remove(gender_at);
    issues.push(
        Issue::new(
            index,
            Severity::Warning,
            category::PLURAL,
            "number_gender_plural",
            params(&[
                ("noun", &words[noun_index].text),
                ("gender", &gender),
                ("plural", &plural),
                ("fixed", &fixed),
                ("current", &current),
            ]),
            vec![fixed.clone()],
        )
        .ending_at(analysis.utf16_offsets[words[noun_index].end]),
    );
}

/// Number before noun `noun_index` (`label` = the number as written): flag a singular noun that
/// has a plural.
fn check_number_plural(
    analysis: &Analysis,
    noun_index: usize,
    label: &str,
    issues: &mut Vec<Issue>,
) {
    let dictionary = analysis.dictionary;
    let noun_form = analysis.words[noun_index].form.as_str();
    if UNITS.contains(&noun_form) {
        return;
    }
    // "zwou Slow Motions": another capitalised word right after, the noun is a multi-word name
    if analysis.next_is_adjacent(noun_index) && starts_upper(&analysis.words[noun_index + 1].text) {
        return;
    }
    let entries = dictionary.entry_indices(analysis.words[noun_index].record);
    let singular = !entries.is_empty()
        && entries.iter().all(|&i| {
            let entry = &dictionary.entries[i];
            entry.part_of_speech == "SUBST"
                && entry.lemma == noun_form
                && !entry.gender.is_empty()
                && !entry.plural.is_empty()
                && !entry.plural.split(", ").any(|plural| plural == noun_form)
        });
    let (true, Some(&first)) = (singular, entries.first()) else {
        return;
    };
    let plural = dictionary.entries[first]
        .plural
        .split(", ")
        .next()
        .unwrap_or("");
    // adjectival nouns (Deputéierten / Deputéiert) look singular but are not
    if plural.is_empty() || plural.contains(' ') || noun_form.starts_with(plural) {
        return;
    }
    issues.push(Issue::new(
        noun_index,
        Severity::Warning,
        category::PLURAL,
        "number_plural",
        params(&[("number", label), ("plural", plural)]),
        vec![plural.to_string()],
    ));
}

/// Preposition + "dem" before a noun: "vun dem Bësch" → "vum Bësch".
fn check_contraction(analysis: &Analysis, index: usize, issues: &mut Vec<Issue>) {
    let words = analysis.words;
    let lowercase = analysis.lowercase(index);
    let Some(&(_, contracted)) = CONTRACTIONS
        .iter()
        .find(|(preposition, _)| *preposition == lowercase)
    else {
        return;
    };
    if !analysis.next_is_adjacent(index) || analysis.lowercase(index + 1) != "dem" {
        return;
    }
    let Some(noun_index) = analysis.noun_after(index + 1) else {
        return;
    };
    // "vun dem Mann säin Auto": "dem" belongs to the possessive, not to the preposition
    let possessive = analysis.next_is_adjacent(noun_index)
        && POSSESSIVES.contains(&analysis.lowercase(noun_index + 1).as_str());
    if possessive {
        return;
    }
    let text = words[index].text.as_str();
    let phrase = analysis.slice(words[index].start, words[index + 1].end);
    let contracted = keep_case(text, contracted);
    issues.push(
        Issue::new(
            index,
            Severity::Warning,
            category::CONTRACTION,
            "contraction",
            params(&[("phrase", &phrase), ("contracted", &contracted)]),
            vec![contracted],
        )
        .ending_at(analysis.utf16_offsets[words[index + 1].end]),
    );
}

/// "ze" + finite verb: "ze spillt" → "ze spillen".
fn check_ze_infinitive(analysis: &Analysis, index: usize, issues: &mut Vec<Issue>) {
    let dictionary = analysis.dictionary;
    if analysis.lowercase(index) != "ze" || !analysis.next_is_adjacent(index) {
        return;
    }
    let verb = &analysis.words[index + 1];
    if starts_upper(&verb.text)
        || verb.status != Status::Ok
        || !analysis.only_part_of_speech(verb, "VRB")
    {
        return;
    }
    // a finite form (not "wild": infinitives and participles are flagged as wild)
    let Some(&(_, false)) = dictionary.person_masks.get(verb.form.as_str()) else {
        return;
    };
    let Some(&first) = dictionary.entry_indices(verb.record).first() else {
        return;
    };
    let lemma = dictionary.entries[first].lemma;
    drop_n_rule(issues, index + 1);
    issues.push(Issue::new(
        index + 1,
        Severity::Error,
        category::VERB,
        "ze_infinitive",
        params(&[("verb", lemma)]),
        vec![lemma.to_string()],
    ));
}

/// The same word twice: "de Mann Mann".
fn check_repeated_word(analysis: &Analysis, index: usize, issues: &mut Vec<Issue>) {
    let dictionary = analysis.dictionary;
    let words = analysis.words;
    let word = &words[index];
    // interjections and adverbs are often doubled on purpose ("hopp hopp", "ganz ganz")
    let emphatic = dictionary
        .entry_indices(word.record)
        .iter()
        .any(|&i| matches!(dictionary.entries[i].part_of_speech, "INTERJ" | "ADV"));
    if analysis.next_is_adjacent(index)
        && !word.clitic
        && !emphatic
        && word.text.chars().count() >= 4
        && analysis.lowercase(index + 1) == analysis.lowercase(index)
        && !words[index + 1].clitic
    {
        issues.push(Issue::new(
            index + 1,
            Severity::Warning,
            category::TYPOGRAPHY,
            "repeated_word",
            params(&[("word", &words[index + 1].text)]),
            vec![String::new()],
        ));
    }
}

/// A lower-case letter at the start of a sentence (not after an abbreviation such as "z. B.").
fn check_capital_start(analysis: &Analysis, index: usize, issues: &mut Vec<Issue>) {
    let words = analysis.words;
    let word = &words[index];
    if !analysis.starts[index].strict {
        return;
    }
    let first = word.text.chars().next().unwrap_or(' ');
    let after_abbreviation = index > 0 && {
        let previous = &words[index - 1];
        previous.text.chars().count() <= 2
            || ABBREVIATIONS.contains(&analysis.lowercase(index - 1).as_str())
    };
    // "(ech…" or "„ech…": only a word preceded by a space or the start of the text
    let bare_start = word.start == 0 || is_space(analysis.chars[word.start - 1]);
    if !is_letter(first) || !first.is_lowercase() || after_abbreviation || !bare_start {
        return;
    }
    let fixed = uppercase_first(&word.text);
    // other fixes on this word keep the capital ("hien" → "Hie")
    for issue in issues.iter_mut().filter(|issue| issue.word_index == index) {
        for fix in issue.fixes.iter_mut() {
            *fix = uppercase_first(fix);
        }
    }
    issues.push(Issue::new(
        index,
        Severity::Warning,
        category::CAPITAL,
        "capital_start",
        params(&[("fixed", &fixed)]),
        vec![fixed],
    ));
}

/// Digits, then a singular noun: "3 Auto" → "3 Autoen".
fn check_digits_plural(
    analysis: &Analysis,
    tokens: &[Token],
    word_at: &Map<usize, usize>,
    issues: &mut Vec<Issue>,
) {
    let words = analysis.words;
    for (token_index, number) in tokens.iter().enumerate() {
        if number.kind != TokenKind::Number {
            continue;
        }
        let digits: String = analysis.chars[number.start..number.end]
            .iter()
            .filter(|c| c.is_ascii_digit() || **c == ',')
            .collect();
        // 0, 1 and decimals ("1,5 Liter") take the singular
        let plural_number = !digits.contains(',') && digits.parse::<u64>().map_or(true, |v| v >= 2);
        if !plural_number {
            continue;
        }
        let (Some(space), Some(next)) = (tokens.get(token_index + 1), tokens.get(token_index + 2))
        else {
            continue;
        };
        if space.kind != TokenKind::Space
            || !is_blank(&analysis.chars[space.start..space.end])
            || next.kind != TokenKind::Word
        {
            continue;
        }
        let Some(&first) = word_at.get(&next.start) else {
            continue;
        };
        let word = &words[first];
        // the noun itself, or one adjective then the noun
        let noun_index = if word.status == Status::Ok && !starts_upper(&word.text) {
            analysis.noun_after(first)
        } else if word.status == Status::Ok
            && starts_upper(&word.text)
            && analysis.only_part_of_speech(word, "SUBST")
        {
            Some(first)
        } else {
            None
        };
        if let Some(noun_index) = noun_index {
            let label = analysis.slice(number.start, number.end);
            check_number_plural(analysis, noun_index, &label, issues);
        }
    }
}

/// A space before , . ; : ? ! ("Moien !" → "Moien!").
fn check_space_before_punctuation(
    analysis: &Analysis,
    tokens: &[Token],
    word_at: &Map<usize, usize>,
    issues: &mut Vec<Issue>,
) {
    let chars = analysis.chars;
    for (token_index, space) in tokens.iter().enumerate() {
        if space.kind != TokenKind::Space
            || token_index == 0
            || !is_blank(&chars[space.start..space.end])
        {
            continue;
        }
        let (Some(before), Some(after)) =
            (tokens.get(token_index - 1), tokens.get(token_index + 1))
        else {
            continue;
        };
        if before.kind != TokenKind::Word || after.kind != TokenKind::Punctuation {
            continue;
        }
        let mark = chars[after.start];
        // "?!", "!!" and "..." are left alone, and so is a mark glued to the next word
        let repeated = chars.get(after.end).is_some_and(|&c| c == mark || c == '.');
        let end_or_space = chars
            .get(after.end)
            .is_none_or(|&c| is_space(c) || matches!(c, '"' | '\u{201c}' | '\u{201d}' | '\u{bb}'));
        if !matches!(mark, ',' | '.' | ';' | ':' | '?' | '!') || repeated || !end_or_space {
            continue;
        }
        let Some(&word_index) = word_at.get(&before.start) else {
            continue;
        };
        let word = &analysis.words[word_index];
        if word.end != before.end {
            continue;
        }
        let mark = mark.to_string();
        issues.push(
            Issue::new(
                word_index,
                Severity::Warning,
                category::TYPOGRAPHY,
                "space_before",
                params(&[("mark", &mark)]),
                vec![format!("{}{mark}", word.text)],
            )
            .ending_at(analysis.utf16_offsets[after.end]),
        );
    }
}

fn analyze_text(dictionary: &Dictionary, text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut utf16_offsets: Vec<usize> = Vec::with_capacity(chars.len() + 1);
    let mut offset = 0;
    for &c in &chars {
        utf16_offsets.push(offset);
        offset += c.len_utf16();
    }
    utf16_offsets.push(offset);

    let tokens = tokenize(dictionary, &chars);
    let (words, starts) = classify_words(dictionary, &chars, &tokens);
    let analysis = Analysis {
        dictionary,
        chars: &chars,
        utf16_offsets: &utf16_offsets,
        words: &words,
        starts: &starts,
    };
    let mut issues: Vec<Issue> = Vec::new();

    // The order of the checks matters: a later rule may drop or adjust an earlier remark on the
    // same word, and remarks on one word keep the order in which they were found.
    for (index, word) in words.iter().enumerate() {
        check_spelling(&analysis, index, &mut issues);
        // the other first-pass rules need a word the LOD knows
        if word.record.is_none() && !word.clitic {
            continue;
        }
        check_n_rule(&analysis, index, &mut issues);
        check_determiner(&analysis, index, &mut issues);
        check_ech_du_verb(&analysis, index, &mut issues);
    }
    for index in 0..words.len() {
        check_pronoun_verb(&analysis, index, &mut issues);
        check_number_word(&analysis, index, &mut issues);
        check_contraction(&analysis, index, &mut issues);
        check_ze_infinitive(&analysis, index, &mut issues);
        check_repeated_word(&analysis, index, &mut issues);
        check_capital_start(&analysis, index, &mut issues);
    }
    let word_at: Map<usize, usize> = words
        .iter()
        .enumerate()
        .map(|(index, word)| (word.start, index))
        .collect();
    check_digits_plural(&analysis, &tokens, &word_at, &mut issues);
    check_space_before_punctuation(&analysis, &tokens, &word_at, &mut issues);

    fit_fixes_to_n_rule(&analysis, &mut issues);
    issues.sort_by_key(|issue| utf16_offsets[words[issue.word_index].start]);
    let json = write_json(&tokens, &words, &issues, dictionary, &utf16_offsets);
    // SAFETY: rules 1 and 2 of the module state: no `last_word()` reference is alive during `analyze`.
    unsafe {
        LAST_WORDS = words;
    }
    json
}

/// The result of `analyze`, as described in the README.
fn write_json(
    tokens: &[Token],
    words: &[Word],
    issues: &[Issue],
    dictionary: &Dictionary,
    utf16_offsets: &[usize],
) -> String {
    // Per-word severity: the most serious remark. Clitics get one but stay out of the counts.
    let mut word_severity = vec![0u8; words.len()];
    for issue in issues {
        let current = &mut word_severity[issue.word_index];
        let code = issue.severity.code();
        if *current == 0 || code < *current {
            *current = code;
        }
    }
    let (mut word_count, mut known, mut a1, mut a2) = (0usize, 0usize, 0usize, 0usize);
    for word in words.iter().filter(|word| !word.clitic) {
        word_count += 1;
        if word.status == Status::Ok || word.status == Status::Case {
            known += 1;
            match dictionary.level(word.record) {
                1 => a1 += 1,
                2 => a2 += 1,
                _ => {}
            }
        }
    }
    let count = |severity: Severity| {
        issues
            .iter()
            .filter(|issue| issue.severity == severity)
            .count()
    };

    let mut out = String::with_capacity(tokens.len() * 8 + issues.len() * 160 + 128);
    out.push_str("{\"k\":\"");
    for token in tokens {
        out.push(match token.kind {
            TokenKind::Word => 'w',
            TokenKind::Space => 's',
            TokenKind::Punctuation => 'p',
            TokenKind::Number => 'n',
        });
    }
    out.push_str("\",\"e\":[");
    for (i, token) in tokens.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        push_number(&mut out, utf16_offsets[token.end]);
    }
    out.push_str("],\"w\":[");
    for (i, word) in words.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('[');
        push_number(&mut out, word.status as usize);
        out.push(',');
        let flags = (word.clitic as usize) | ((word.record.is_some() as usize) << 1);
        push_number(&mut out, flags);
        out.push(',');
        push_number(&mut out, word_severity[i] as usize);
        if word.form != word.text {
            out.push(',');
            push_json_string(&mut out, &word.form);
        }
        out.push(']');
    }
    out.push_str("],\"i\":[");
    for (i, issue) in issues.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('[');
        push_number(&mut out, issue.word_index);
        out.push(',');
        push_json_string(&mut out, issue.severity.as_str());
        out.push(',');
        push_json_string(&mut out, issue.category);
        out.push(',');
        push_json_string(&mut out, issue.rule);
        out.push_str(",{");
        for (n, (name, value)) in issue.params.iter().enumerate() {
            if n > 0 {
                out.push(',');
            }
            push_json_string(&mut out, name);
            out.push(':');
            push_json_string(&mut out, value);
        }
        out.push_str("},");
        push_json_array(&mut out, &issue.fixes);
        if let Some(end) = issue.end {
            out.push(',');
            push_number(&mut out, end);
        }
        out.push(']');
    }
    out.push_str("],\"s\":[");
    let stats = [
        word_count,
        known,
        a1,
        a2,
        count(Severity::Error),
        count(Severity::Warning),
        count(Severity::Info),
    ];
    for (i, value) in stats.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        push_number(&mut out, value);
    }
    out.push_str("]}");
    out
}

fn describe_word(dictionary: &Dictionary, word: &Word) -> String {
    let records: Vec<Option<FormRecord>> = match &word.part_records {
        Some(parts) => parts.clone(),
        None => vec![word.record],
    };
    let mut seen: Vec<usize> = Vec::new();
    let mut out = String::from("[");
    for rec in records {
        for i in dictionary.entry_indices(rec) {
            if seen.contains(&i) {
                continue;
            }
            seen.push(i);
            let entry = &dictionary.entries[i];
            if out.len() > 1 {
                out.push(',');
            }
            out.push_str("{\"id\":");
            push_json_string(&mut out, entry.id);
            out.push_str(",\"lemma\":");
            push_json_string(&mut out, entry.lemma);
            out.push_str(",\"pos\":");
            push_json_string(&mut out, entry.part_of_speech);
            out.push_str(",\"gen\":");
            push_json_string(&mut out, entry.gender);
            out.push_str(",\"lvl\":");
            push_number(&mut out, entry.level as usize);
            for (language, text) in LANGUAGES.iter().zip(entry.translations) {
                out.push_str(",\"");
                out.push_str(language);
                out.push_str("\":");
                push_json_string(&mut out, text);
            }
            out.push_str(",\"plural\":");
            push_json_string(&mut out, entry.plural);
            out.push_str(",\"conj\":");
            match dictionary.conjugations.get(&(i as u32)) {
                Some(c) => push_json_array(&mut out, &c[..]),
                None => out.push_str("null"),
            }
            out.push('}');
        }
    }
    out.push(']');
    out
}

// ---------- exports ----------
fn dictionary() -> Option<&'static Dictionary> {
    let ptr = &raw const DICTIONARY;
    // SAFETY: rules 1 and 2 of the module state. DB is only replaced by `init`.
    unsafe { &*ptr }.as_ref()
}
/// Word `wi` of the latest `analyze()`.
fn last_word(word_index: u32) -> Option<&'static Word> {
    let ptr = &raw const LAST_WORDS;
    // SAFETY: rules 1 and 2 of the module state. LAST_WORDS is only replaced by `analyze` and `init`.
    unsafe { &*ptr }.get(word_index as usize)
}
/// The UTF-8 text JS wrote at `ptr`, or None if it is not valid UTF-8.
///
/// # Safety
/// `ptr` must point to `len` readable bytes that stay unchanged for `'a`.
unsafe fn input_text<'a>(ptr: *const u8, len: usize) -> Option<&'a str> {
    if len == 0 {
        return Some("");
    }
    // SAFETY: guaranteed by the caller, see above.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    std::str::from_utf8(bytes).ok()
}

#[no_mangle]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    let mut buffer = Vec::<u8>::with_capacity(len.max(1));
    let pointer = buffer.as_mut_ptr();
    std::mem::forget(buffer);
    pointer
}

/// # Safety
/// `ptr`/`len` must come from `alloc(len)`.
#[no_mangle]
pub unsafe extern "C" fn dealloc(ptr: *mut u8, len: usize) {
    // SAFETY: `alloc(len)` created a Vec<u8> of capacity `len.max(1)` at `ptr` and forgot it;
    // rebuilding it with length 0 frees the memory without reading it.
    drop(unsafe { Vec::from_raw_parts(ptr, 0, len.max(1)) });
}

/// Loads lod.txt. Takes ownership of the buffer (it stays alive as the string table).
/// Returns 0, or 1 = not UTF-8, 2..7 = malformed header / entries / forms / attested / masks /
/// conjugations, 8 = malformed spellchecker.lu sections.
/// # Safety
/// `ptr`/`len` must come from `alloc(len)` and must not be freed by the caller.
#[no_mangle]
pub unsafe extern "C" fn init(ptr: *mut u8, len: usize) -> i32 {
    // SAFETY: the buffer comes from `alloc(len)` (capacity `len.max(1)`), JS filled its `len`
    // bytes, and from here on the module owns it.
    let bytes = unsafe { Vec::from_raw_parts(ptr, len, len.max(1)) };
    let Ok(text) = String::from_utf8(bytes) else {
        return 1;
    };
    let text: StaticStr = Box::leak(text.into_boxed_str());
    match load_dictionary(text) {
        Ok(d) => {
            // SAFETY: rules 1 and 2 of the module state: no reference into the old state is alive.
            unsafe {
                DICTIONARY = Some(d);
                FOLD_INDEX = None;
                LAST_WORDS = Vec::new();
            }
            0
        }
        Err(code) => code,
    }
}

#[no_mangle]
pub extern "C" fn version() -> *const u8 {
    output(dictionary().map_or("", |d| d.version))
}

/// 1 when the spellchecker.lu dictionary accepts the word with Hunspell's case rules (a capital
/// first letter is always allowed), 0 otherwise or without that dictionary. Used by
/// test/spell.mjs against the real hunspell.
///
/// # Safety
/// `ptr`/`len` must describe readable memory.
#[no_mangle]
pub unsafe extern "C" fn spell(ptr: *const u8, len: usize) -> i32 {
    // SAFETY: guaranteed by the caller, see above; the text is not used after this call.
    match (dictionary(), unsafe { input_text(ptr, len) }) {
        (Some(loaded), Some(word)) => i32::from(loaded.speller.check_any_case(word)),
        _ => 0,
    }
}

/// Plain text: the value of word `wi` when it is a number written in words ("117649"), or "".
#[no_mangle]
pub extern "C" fn number(word_index: u32) -> *const u8 {
    let value = last_word(word_index)
        .and_then(|word| word.number.as_ref())
        .map(|reading| reading.value.to_string())
        .unwrap_or_default();
    output(&value)
}

/// Number of dictionary entries loaded (0 before `init`).
#[no_mangle]
pub extern "C" fn entries() -> i32 {
    dictionary().map_or(0, |d| d.entries.len() as i32)
}

/// # Safety
/// `ptr`/`len` must describe readable memory.
#[no_mangle]
pub unsafe extern "C" fn analyze(ptr: *const u8, len: usize) -> *const u8 {
    // SAFETY: guaranteed by the caller, see above; the text is not used after this call.
    let (Some(loaded), Some(text)) = (dictionary(), unsafe { input_text(ptr, len) }) else {
        return output("{\"error\":1}");
    };
    output(&analyze_text(loaded, text))
}

#[no_mangle]
pub extern "C" fn describe(word_index: u32) -> *const u8 {
    match (dictionary(), last_word(word_index)) {
        (Some(loaded), Some(word)) => output(&describe_word(loaded, word)),
        _ => output("[]"),
    }
}

/// Plain text (not JSON). lang: 0 = French, 1 = English, 2 = German, 3 = Portuguese.
#[no_mangle]
pub extern "C" fn gloss(word_index: u32, lang: u32) -> *const u8 {
    match (dictionary(), last_word(word_index)) {
        (Some(loaded), Some(word)) if word.record.is_some() => {
            output(loaded.gloss(word.record, (lang as usize).min(LANGUAGES.len() - 1)))
        }
        _ => output(""),
    }
}

#[no_mangle]
pub extern "C" fn level(word_index: u32) -> i32 {
    match (dictionary(), last_word(word_index)) {
        (Some(loaded), Some(word)) => loaded.level(word.record) as i32,
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn nfull(word_index: u32) -> *const u8 {
    let mut out = String::new();
    let full_forms: Vec<&str> = match last_word(word_index).and_then(|word| word.record) {
        Some(r) if !r.full_forms.is_empty() => r.full_forms.split('|').collect(),
        _ => Vec::new(),
    };
    push_json_array(&mut out, &full_forms);
    output(&out)
}

/// Plain text (not JSON): the clitic word `wi` is ("s", "d'", "z'", "'t", "'n", "'s"), or "".
#[no_mangle]
pub extern "C" fn clitic(word_index: u32) -> *const u8 {
    output(
        last_word(word_index)
            .filter(|word| word.clitic)
            .map_or("", |word| word.form.as_str()),
    )
}

/// # Safety
/// `ptr`/`len` must describe readable memory.
#[no_mangle]
pub unsafe extern "C" fn suggest(ptr: *const u8, len: usize, max: u32) -> *const u8 {
    let mut out = String::new();
    // SAFETY: guaranteed by the caller, see above; the text is not used after this call.
    let list = match (dictionary(), unsafe { input_text(ptr, len) }) {
        (Some(loaded), Some(word)) => loaded.suggest(word, max as usize),
        _ => Vec::new(),
    };
    push_json_array(&mut out, &list);
    output(&out)
}

/// Test hook: the folded key of a string as a JSON array of UTF-16 units.
/// # Safety
/// `ptr`/`len` must describe readable memory.
#[no_mangle]
pub unsafe extern "C" fn fold(ptr: *const u8, len: usize) -> *const u8 {
    let mut out = String::from("[");
    // SAFETY: guaranteed by the caller, see above; the text is not used after this call.
    if let (Some(loaded), Some(word)) = (dictionary(), unsafe { input_text(ptr, len) }) {
        for (i, u) in loaded.fold(word).into_iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            push_number(&mut out, u as usize);
        }
    }
    out.push(']');
    output(&out)
}
