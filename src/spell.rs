// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Word check with the spellchecker.lu Hunspell dictionary for Luxembourgish (Zenter fir
//! d'Lëtzebuerger Sprooch, CC0). It knows many correct words the LOD has no entry for: derived
//! adjectives, compounds, names, inflected forms.
//!
//! This is a reader for the part of the Hunspell affix format that dictionary uses, not a general
//! Hunspell: two-character flags (FLAG long), prefixes and suffixes with conditions, cross
//! products, continuation classes (a second suffix on top of the first, as in
//! "superlative + inflection"), CIRCUMFIX (a prefix and a suffix that only exist together, as in
//! the participle "ge…t"), NOSUGGEST and FULLSTRIP. No compounding: the dictionary has none.
//! test/spell.mjs compares it with the real hunspell on many words.

use crate::{Map, StaticStr};

type Flag = u16;

fn flag_of(two: &[u8]) -> Flag {
    (u16::from(two[0]) << 8) | u16::from(two[1])
}

/// Whether a flag string ("v0X0g1w0", two ASCII characters per flag) holds `flag`.
fn has_flag(flags: &str, flag: Flag) -> bool {
    flags
        .as_bytes()
        .chunks_exact(2)
        .any(|two| flag_of(two) == flag)
}

/// One position of an affix condition: any character, or a set of characters (maybe negated).
enum ConditionPart {
    Any,
    Set {
        negated: bool,
        characters: Vec<char>,
    },
}

impl ConditionPart {
    fn matches(&self, character: char) -> bool {
        match self {
            ConditionPart::Any => true,
            ConditionPart::Set {
                negated,
                characters,
            } => characters.contains(&character) != *negated,
        }
    }
}

fn parse_condition(condition: &str) -> Vec<ConditionPart> {
    let mut parts = Vec::new();
    let mut characters = condition.chars();
    while let Some(character) = characters.next() {
        match character {
            '.' => parts.push(ConditionPart::Any),
            '[' => {
                let mut set: Vec<char> = Vec::new();
                let mut negated = false;
                for inside in characters.by_ref() {
                    match inside {
                        ']' => break,
                        '^' if set.is_empty() && !negated => negated = true,
                        _ => set.push(inside),
                    }
                }
                parts.push(ConditionPart::Set {
                    negated,
                    characters: set,
                });
            }
            _ => parts.push(ConditionPart::Set {
                negated: false,
                characters: vec![character],
            }),
        }
    }
    parts
}

struct Affix {
    flag: Flag,
    cross_product: bool,
    strip: StaticStr,
    append: StaticStr,
    /// Flags this affix carries: a suffix that may follow it, or the circumfix mark.
    continuation: StaticStr,
    condition: Vec<ConditionPart>,
}

impl Affix {
    /// The condition holds at the start of a stem (prefixes).
    fn fits_start(&self, stem: &str) -> bool {
        let mut characters = stem.chars();
        self.condition
            .iter()
            .all(|part| characters.next().is_some_and(|c| part.matches(c)))
    }
    /// The condition holds at the end of a stem (suffixes).
    fn fits_end(&self, stem: &str) -> bool {
        let mut characters = stem.chars().rev();
        self.condition
            .iter()
            .rev()
            .all(|part| characters.next().is_some_and(|c| part.matches(c)))
    }
}

pub(crate) struct Speller {
    pub(crate) version: StaticStr,
    /// Word -> the flag strings of its homonyms.
    words: Map<StaticStr, Vec<StaticStr>>,
    /// Affixes by the text they add.
    prefixes: Map<StaticStr, Vec<Affix>>,
    suffixes: Map<StaticStr, Vec<Affix>>,
    longest_prefix: usize,
    longest_suffix: usize,
    circumfix: Option<Flag>,
    no_suggest: Option<Flag>,
    full_strip: bool,
    /// Typical confusions, wrong -> right ("REP ae ä"); "_" is a space.
    pub(crate) replacements: Vec<(StaticStr, StaticStr)>,
    /// Keyboard rows, for letters typed with a neighbouring key.
    pub(crate) keyboard: Vec<Vec<char>>,
    /// Letters to try when looking for a correction, most frequent first.
    try_characters: Vec<char>,
    /// Flags that some affix allows after it (continuation classes, the circumfix mark aside):
    /// only those suffixes can sit on top of another one.
    continued: Vec<Flag>,
}

impl Speller {
    /// An empty speller (no dictionary): accepts nothing.
    pub(crate) fn empty() -> Speller {
        Speller {
            version: "-",
            words: Map::default(),
            prefixes: Map::default(),
            suffixes: Map::default(),
            longest_prefix: 0,
            longest_suffix: 0,
            circumfix: None,
            no_suggest: None,
            full_strip: false,
            replacements: Vec::new(),
            keyboard: Vec::new(),
            try_characters: Vec::new(),
            continued: Vec::new(),
        }
    }

    /// Reads the affix lines and the word lines of lod.txt (see tools/convert.mjs).
    pub(crate) fn load(
        version: StaticStr,
        affix_lines: &[StaticStr],
        word_lines: &[StaticStr],
    ) -> Option<Speller> {
        let mut speller = Speller::empty();
        speller.version = version;
        let mut cross_products: Map<(bool, Flag), bool> = Map::default();
        for line in affix_lines {
            let fields: Vec<StaticStr> = line.split(' ').collect();
            let flag_field = |index: usize| -> Option<Flag> {
                let bytes = fields.get(index)?.as_bytes();
                (bytes.len() == 2).then(|| flag_of(bytes))
            };
            match fields[0] {
                "CIRCUMFIX" => speller.circumfix = Some(flag_field(1)?),
                "NOSUGGEST" => speller.no_suggest = Some(flag_field(1)?),
                "FULLSTRIP" => speller.full_strip = true,
                "KEY" => {
                    speller.keyboard = fields
                        .get(1)?
                        .split('|')
                        .map(|row| row.chars().collect())
                        .collect()
                }
                "REP" if fields.len() == 3 => speller.replacements.push((fields[1], fields[2])),
                "TRY" => speller.try_characters = fields.get(1)?.chars().collect(),
                "PFX" | "SFX" => {
                    let is_prefix = fields[0] == "PFX";
                    let flag = flag_field(1)?;
                    if fields.len() == 4 {
                        cross_products.insert((is_prefix, flag), fields[2] == "Y");
                        continue;
                    }
                    if fields.len() != 5 {
                        return None;
                    }
                    let (append, continuation) =
                        fields[3].split_once('/').unwrap_or((fields[3], ""));
                    let empty_if_zero = |text: StaticStr| if text == "0" { "" } else { text };
                    let affix = Affix {
                        flag,
                        cross_product: cross_products.get(&(is_prefix, flag)) == Some(&true),
                        strip: empty_if_zero(fields[2]),
                        append: empty_if_zero(append),
                        continuation,
                        condition: parse_condition(fields[4]),
                    };
                    let length = affix.append.len();
                    let (table, longest) = if is_prefix {
                        (&mut speller.prefixes, &mut speller.longest_prefix)
                    } else {
                        (&mut speller.suffixes, &mut speller.longest_suffix)
                    };
                    *longest = (*longest).max(length);
                    table.entry(affix.append).or_default().push(affix);
                }
                _ => {}
            }
        }
        // Affixes with the same strip next to each other: one stem per run (see suffix_check).
        for affixes in speller
            .prefixes
            .values_mut()
            .chain(speller.suffixes.values_mut())
        {
            affixes.sort_by_key(|affix| affix.strip);
        }
        let mut continued: Vec<Flag> = speller
            .prefixes
            .values()
            .chain(speller.suffixes.values())
            .flatten()
            .flat_map(|affix| affix.continuation.as_bytes().chunks_exact(2).map(flag_of))
            .filter(|&flag| Some(flag) != speller.circumfix)
            .collect();
        continued.sort_unstable();
        continued.dedup();
        speller.continued = continued;
        speller.words.reserve(word_lines.len());
        for line in word_lines {
            let (word, flags) = line.split_once('\t')?;
            speller.words.entry(word).or_default().push(flags);
        }
        Some(speller)
    }

    fn is_circumfix(&self, affix: &Affix) -> bool {
        self.circumfix
            .is_some_and(|flag| has_flag(affix.continuation, flag))
    }

    /// Some homonym of `root` satisfies `test` (given its flags).
    fn root_with(&self, root: &str, test: impl Fn(StaticStr) -> bool) -> bool {
        self.words
            .get(root)
            .is_some_and(|homonyms| homonyms.iter().any(|&flags| test(flags)))
    }

    /// The word is spelt right according to the dictionary (case-sensitive, as Hunspell's own
    /// lookup; the caller tries the case variants).
    pub(crate) fn check(&self, word: &str) -> bool {
        if word.is_empty() {
            return false;
        }
        self.words.contains_key(word) || self.suffix_check(word, None) || self.prefix_check(word)
    }

    /// Hunspell's own case rules on top of `check`: a capital first letter is always allowed
    /// ("Fir" at a sentence start), and an all-capitals word may be any casing ("FIR").
    pub(crate) fn check_any_case(&self, word: &str) -> bool {
        if self.check(word) {
            return true;
        }
        let mut characters = word.chars();
        let Some(first) = characters.next() else {
            return false;
        };
        let rest = characters.as_str();
        let lower_first = || first.to_lowercase().collect::<String>() + rest;
        if first.is_uppercase() && self.check(&lower_first()) {
            return true;
        }
        if word.chars().count() > 1 && word.chars().all(|c| !c.is_lowercase()) {
            let capitalised = first.to_string() + &rest.to_lowercase();
            return self.check(&capitalised) || self.check(&word.to_lowercase());
        }
        false
    }

    /// Like `check`, but false for words the dictionary marks as never to be suggested.
    pub(crate) fn check_suggestible(&self, word: &str) -> bool {
        if let Some(homonyms) = self.words.get(word) {
            if let Some(no_suggest) = self.no_suggest {
                if homonyms.iter().all(|flags| has_flag(flags, no_suggest)) {
                    return false;
                }
            }
            return true;
        }
        self.check(word)
    }

    /// Spellings one typo away from `word` that the dictionary accepts and may suggest: a letter
    /// deleted, two neighbours swapped, a letter replaced or inserted (from the TRY letters), and
    /// the dictionary's typical confusions (REP). Case as written.
    pub(crate) fn corrections(&self, word: &str) -> Vec<String> {
        const LETTERS_TRIED: usize = 32;
        let characters: Vec<char> = word.chars().collect();
        // Lower-case letters only: a capital is not a typo inside a word.
        let letters: Vec<char> = self
            .try_characters
            .iter()
            .copied()
            .filter(|c| c.is_lowercase())
            .take(LETTERS_TRIED)
            .collect();
        let mut candidates: Vec<String> = Vec::new();
        let rebuild = |parts: &[char]| parts.iter().collect::<String>();
        for at in 0..=characters.len() {
            if at < characters.len() {
                let mut deleted = characters.clone();
                deleted.remove(at);
                candidates.push(rebuild(&deleted));
                for &letter in &letters {
                    if letter != characters[at] {
                        let mut replaced = characters.clone();
                        replaced[at] = letter;
                        candidates.push(rebuild(&replaced));
                    }
                }
            }
            if at + 1 < characters.len() && characters[at] != characters[at + 1] {
                let mut swapped = characters.clone();
                swapped.swap(at, at + 1);
                candidates.push(rebuild(&swapped));
            }
            for &letter in &letters {
                let mut inserted = characters.clone();
                inserted.insert(at, letter);
                candidates.push(rebuild(&inserted));
            }
        }
        candidates.extend(self.replacement_candidates(word));
        // Hunspell's case rule: a capital first letter is always allowed, so "Allebéid" is right
        // when the dictionary has "allebéid".
        let suggestible = |candidate: &str| {
            self.check_suggestible(candidate)
                || candidate.chars().next().is_some_and(char::is_uppercase) && {
                    let mut characters = candidate.chars();
                    let first: String = characters
                        .next()
                        .into_iter()
                        .flat_map(char::to_lowercase)
                        .collect();
                    self.check_suggestible(&(first + characters.as_str()))
                }
        };
        let mut tried: crate::Set<String> = crate::Set::default();
        let mut found: Vec<String> = Vec::new();
        for candidate in candidates {
            if candidate != word
                && !candidate.contains(' ')
                && tried.insert(candidate.clone())
                && suggestible(&candidate)
            {
                found.push(candidate);
            }
        }
        found
    }

    /// `word` with one of the dictionary's typical confusions corrected ("REP ae ä": "aerger" ->
    /// "ärger"); "_" in a replacement is a space.
    pub(crate) fn replacement_candidates(&self, word: &str) -> Vec<String> {
        let mut out = Vec::new();
        for &(wrong, right) in &self.replacements {
            let right = right.replace('_', " ");
            let mut from = 0;
            while let Some(found) = word[from..].find(wrong) {
                let at = from + found;
                out.push(format!(
                    "{}{}{}",
                    &word[..at],
                    right,
                    &word[at + wrong.len()..]
                ));
                from = at + wrong.len().max(1);
                if from > word.len() {
                    break;
                }
            }
        }
        out
    }

    /// Two letters sit next to each other on the keyboard (KEY rows).
    pub(crate) fn keyboard_neighbours(&self, a: char, b: char) -> bool {
        self.keyboard.iter().any(|row| {
            row.windows(2)
                .any(|pair| (pair[0] == a && pair[1] == b) || (pair[0] == b && pair[1] == a))
        })
    }

    /// Byte positions where a split leaves at most `longest` bytes of affix, at char boundaries.
    fn splits(word: &str, longest: usize, from_end: bool) -> impl Iterator<Item = usize> + '_ {
        (0..=word.len()).filter(move |&at| {
            word.is_char_boundary(at)
                && if from_end {
                    word.len() - at <= longest
                } else {
                    at <= longest
                }
        })
    }

    fn can_carry_suffix(&self, flag: Flag) -> bool {
        self.continued.binary_search(&flag).is_ok()
    }

    /// word = root + suffix (or root + inner suffix + suffix), with an optional prefix already
    /// removed from the front. The affixes of one ending are sorted by strip, so the stem and its
    /// dictionary lookup are computed once per strip; an affix whose stem is not a word is skipped
    /// at once unless another suffix may sit under it.
    fn suffix_check(&self, word: &str, prefix: Option<&Affix>) -> bool {
        let mut stem = String::new();
        for at in Speller::splits(word, self.longest_suffix, true) {
            let Some(affixes) = self.suffixes.get(&word[at..]) else {
                continue;
            };
            if at == 0 && !self.full_strip {
                continue;
            }
            let mut current_strip: Option<StaticStr> = None;
            let mut homonyms: Option<&Vec<StaticStr>> = None;
            for suffix in affixes {
                if prefix.is_some() && !suffix.cross_product {
                    continue;
                }
                if current_strip != Some(suffix.strip) {
                    stem.clear();
                    stem.push_str(&word[..at]);
                    stem.push_str(suffix.strip);
                    current_strip = Some(suffix.strip);
                    homonyms = self.words.get(stem.as_str());
                }
                let stacked = self.can_carry_suffix(suffix.flag);
                if (homonyms.is_none() && !stacked) || stem.is_empty() || !suffix.fits_end(&stem) {
                    continue;
                }
                let circumfix = self.is_circumfix(suffix);
                if let Some(homonyms) = homonyms {
                    if circumfix == prefix.is_some_and(|prefix| self.is_circumfix(prefix))
                        && homonyms.iter().any(|&flags| {
                            has_flag(flags, suffix.flag)
                                && prefix.is_none_or(|prefix| {
                                    has_flag(flags, prefix.flag)
                                        || has_flag(suffix.continuation, prefix.flag)
                                })
                        })
                    {
                        return true;
                    }
                }
                if stacked && self.inner_suffix_check(&stem, suffix, prefix) {
                    return true;
                }
            }
        }
        false
    }

    /// stem = root + an inner suffix whose continuation allows `outer` after it.
    fn inner_suffix_check(&self, stem: &str, outer: &Affix, prefix: Option<&Affix>) -> bool {
        let mut root = String::new();
        for at in Speller::splits(stem, self.longest_suffix, true) {
            let Some(affixes) = self.suffixes.get(&stem[at..]) else {
                continue;
            };
            if at == 0 && !self.full_strip {
                continue;
            }
            let mut current_strip: Option<StaticStr> = None;
            let mut homonyms: Option<&Vec<StaticStr>> = None;
            for inner in affixes {
                if !has_flag(inner.continuation, outer.flag)
                    || (prefix.is_some() && !inner.cross_product)
                {
                    continue;
                }
                if current_strip != Some(inner.strip) {
                    root.clear();
                    root.push_str(&stem[..at]);
                    root.push_str(inner.strip);
                    current_strip = Some(inner.strip);
                    homonyms = self.words.get(root.as_str());
                }
                let Some(homonyms) = homonyms else {
                    continue;
                };
                if root.is_empty() || !inner.fits_end(&root) {
                    continue;
                }
                let circumfix = self.is_circumfix(inner) || self.is_circumfix(outer);
                if circumfix == prefix.is_some_and(|prefix| self.is_circumfix(prefix))
                    && homonyms.iter().any(|&flags| {
                        has_flag(flags, inner.flag)
                            && prefix.is_none_or(|prefix| {
                                has_flag(flags, prefix.flag)
                                    || has_flag(inner.continuation, prefix.flag)
                            })
                    })
                {
                    return true;
                }
            }
        }
        false
    }

    /// word = prefix + root, maybe with a suffix too.
    fn prefix_check(&self, word: &str) -> bool {
        for at in Speller::splits(word, self.longest_prefix, false) {
            let Some(affixes) = self.prefixes.get(&word[..at]) else {
                continue;
            };
            for prefix in affixes {
                let stem = format!("{}{}", prefix.strip, &word[at..]);
                if stem.is_empty()
                    || (at == word.len() && !self.full_strip)
                    || (!prefix.cross_product && !self.words.contains_key(stem.as_str()))
                    || !prefix.fits_start(&stem)
                {
                    continue;
                }
                if !self.is_circumfix(prefix)
                    && self.root_with(&stem, |flags| has_flag(flags, prefix.flag))
                {
                    return true;
                }
                if prefix.cross_product && self.suffix_check(&stem, Some(prefix)) {
                    return true;
                }
            }
        }
        false
    }
}
