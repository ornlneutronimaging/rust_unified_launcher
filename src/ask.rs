//! The "Ask" box: a forgiving, ranked search for people who know what they
//! want to do but not what the tool is called — "tiff integrated images
//! over a stack of runs" should land on the Sample Drift Viewer even though
//! its entry never says "tiff" or "stack".
//!
//! No language model involved: every word of the question is normalised
//! (lower-cased, light stemming, stop words dropped) and expanded with the
//! `[ask] synonyms` groups of the config, then looked up in each
//! application's name, tags, `ask_examples` and description. Name / tag /
//! example hits weigh more than description hits, a synonym hit slightly
//! less than a direct one, and the sum over the question's words ranks the
//! applications. Everything runs in memory; nothing leaves the machine.
//!
//! The filter box (`AppEntry::matches`) stays a strict "every word must
//! appear" substring filter: this module only feeds the suggestions listed
//! under the Ask box.

use std::collections::HashSet;

/// Words that carry no meaning for the ranking (checked before stemming).
const STOP_WORDS: &[&str] = &[
    "a", "an", "the", "of", "over", "for", "in", "on", "to", "and", "or",
    "with", "from", "at", "by", "is", "are", "was", "be", "it", "its", "this",
    "that", "these", "those", "my", "me", "i", "we", "our", "you", "want",
    "wants", "need", "needs", "would", "like", "find", "look", "looking",
    "search", "searching", "tool", "tools", "app", "application",
    "applications", "program", "how", "do", "does", "did", "can", "could",
    "should", "what", "which", "where", "when", "who", "some", "any", "all",
    "get", "show", "shows", "open", "use", "using", "used", "into", "via",
    "per", "vs", "about", "one", "something", "right", "please", "help",
    "there", "here", "then", "than", "them", "way", "just", "also", "have",
    "has", "not", "no", "so", "if", "as", "up", "out", "know",
];

/// Score of a word found in an application's name / tags & examples /
/// description. A synonym hit scores one less (never below 1).
const WEIGHT_NAME: u32 = 4;
const WEIGHT_TAG: u32 = 3;
const WEIGHT_DESCRIPTION: u32 = 1;
/// Bonus when every word of the question hits the same `ask_examples`
/// phrase: the entry was written for exactly this question.
const EXAMPLE_COVER_BONUS: u32 = 4;

/// Light stemmer: enough to make "runs" meet "run", "integrated" meet
/// "integrate", "imaging" meet "image" — together with the prefix rule of
/// [`token_match`]. Not a real stemmer, and it does not need to be, since
/// both the question and the catalogue go through it.
pub fn stem(word: &str) -> String {
    let w = word.to_lowercase();
    let n = w.len();
    for (suffix, min_len) in [("ies", 5), ("ing", 6), ("ed", 5), ("es", 5), ("s", 4)] {
        if n >= min_len && w.ends_with(suffix) {
            let base = &w[..n - suffix.len()];
            return if suffix == "ies" {
                format!("{base}y")
            } else {
                base.to_owned()
            };
        }
    }
    w
}

/// Does a question token hit a catalogue token? Equal stems, one a prefix
/// (of at least four characters) of the other — "imag" (from "images")
/// hits "image", "integrat" hits "integration" — or a common prefix of at
/// least six characters, so "normalize" meets "normalization" and
/// "register" meets "registration".
fn token_match(q: &str, h: &str) -> bool {
    if q == h || (q.len() >= 4 && h.starts_with(q)) || (h.len() >= 4 && q.starts_with(h)) {
        return true;
    }
    q.bytes().zip(h.bytes()).take_while(|(a, b)| a == b).count() >= 6
}

/// Lower-case, split on anything that is not a letter or digit, drop stop
/// words and one-character bits, stem the rest.
pub fn normalize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| w.chars().count() >= 2 && !STOP_WORDS.contains(&w.as_str()))
        .map(|w| stem(&w))
        .collect()
}

/// The `[ask] synonyms` groups of the config, normalised.
#[derive(Default, Clone)]
pub struct Synonyms {
    groups: Vec<Vec<String>>,
}

impl Synonyms {
    pub fn new(groups: &[Vec<String>]) -> Self {
        Self {
            groups: groups
                .iter()
                .map(|g| g.iter().map(|w| stem(w.trim())).filter(|w| !w.is_empty()).collect())
                .collect(),
        }
    }

    /// Every stem equivalent to `stem` — not including itself.
    fn equivalents(&self, stem: &str) -> Vec<&str> {
        let mut out = Vec::new();
        for g in &self.groups {
            if g.iter().any(|w| w == stem) {
                out.extend(g.iter().map(String::as_str).filter(|w| *w != stem));
            }
        }
        out
    }
}

/// One application's searchable words, normalised once (see `App::reload`).
pub struct Haystack {
    name: Vec<String>,
    /// Tags and, flattened, the `ask_examples` phrases.
    tags: Vec<String>,
    /// Each `ask_examples` phrase on its own, for the coverage bonus.
    examples: Vec<Vec<String>>,
    description: Vec<String>,
}

impl Haystack {
    pub fn new(name: &str, tags: &[String], examples: &[String], description: &str) -> Self {
        let examples: Vec<Vec<String>> = examples.iter().map(|e| normalize(e)).collect();
        let mut tag_words: Vec<String> = tags.iter().flat_map(|t| normalize(t)).collect();
        tag_words.extend(examples.iter().flatten().cloned());
        Self {
            name: normalize(name),
            tags: tag_words,
            examples,
            description: normalize(description),
        }
    }
}

/// A ranked hit of [`rank`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// The caller's index (position in `cfg.apps`).
    pub index: usize,
    pub score: u32,
    /// The question's words (as typed, lower-cased) that hit this entry.
    pub matched: Vec<String>,
}

/// Rank `haystacks` (caller index, words) against a free-text question.
/// Entries with no hit are left out; the rest come best first (score, then
/// number of words matched, then the caller's order).
pub fn rank<'a>(
    question: &str,
    haystacks: impl IntoIterator<Item = (usize, &'a Haystack)>,
    synonyms: &Synonyms,
) -> Vec<Suggestion> {
    // Each typed word with its stems (a hyphenated word yields several) —
    // the typed form is what the suggestion row shows as "matches …".
    let words: Vec<(String, Vec<String>)> = question
        .split_whitespace()
        .map(|w| (w.to_lowercase(), normalize(w)))
        .filter(|(_, stems)| !stems.is_empty())
        .collect();
    if words.is_empty() {
        return Vec::new();
    }

    let field_hit = |field: &[String], stems: &[String]| -> bool {
        stems.iter().any(|q| field.iter().any(|h| token_match(q, h)))
    };

    let mut out = Vec::new();
    for (index, hay) in haystacks {
        let mut score = 0;
        let mut matched = Vec::new();
        // Words that hit at least one of the entry's example phrases: the
        // coverage bonus needs every word on the same phrase.
        let mut covered_examples: Vec<HashSet<usize>> = Vec::new();
        for (typed, stems) in &words {
            let synonym_stems: Vec<String> = stems
                .iter()
                .flat_map(|s| synonyms.equivalents(s))
                .map(str::to_owned)
                .collect();
            let best = |field: &[String], weight: u32| -> u32 {
                if field_hit(field, stems) {
                    weight
                } else if field_hit(field, &synonym_stems) {
                    weight.saturating_sub(1).max(1)
                } else {
                    0
                }
            };
            let word_score = best(&hay.name, WEIGHT_NAME)
                .max(best(&hay.tags, WEIGHT_TAG))
                .max(best(&hay.description, WEIGHT_DESCRIPTION));
            if word_score > 0 {
                score += word_score;
                matched.push(typed.clone());
                let hits: HashSet<usize> = hay
                    .examples
                    .iter()
                    .enumerate()
                    .filter(|(_, ex)| field_hit(ex, stems) || field_hit(ex, &synonym_stems))
                    .map(|(i, _)| i)
                    .collect();
                covered_examples.push(hits);
            }
        }
        if score == 0 {
            continue;
        }
        if matched.len() == words.len() {
            let all_on_one_example = (0..hay.examples.len())
                .any(|i| covered_examples.iter().all(|set| set.contains(&i)));
            if all_on_one_example {
                score += EXAMPLE_COVER_BONUS;
            }
        }
        out.push(Suggestion { index, score, matched });
    }
    out.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(b.matched.len().cmp(&a.matched.len()))
            .then(a.index.cmp(&b.index))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn stemming_meets_across_forms() {
        assert_eq!(stem("runs"), "run");
        assert_eq!(stem("entries"), "entry");
        assert!(token_match(&stem("images"), &stem("image")));
        assert!(token_match(&stem("integrated"), &stem("integration")));
        assert!(token_match(&stem("imaging"), &stem("image")));
        assert!(!token_match("run", "runtime"));
        assert!(token_match(&stem("normalize"), &stem("normalization")));
        assert!(token_match(&stem("register"), &stem("registration")));
        assert!(!token_match("sample", "sinogram"));
    }

    #[test]
    fn normalize_drops_stop_words_and_splits_hyphens() {
        assert_eq!(normalize("Did the sample drift?"), s(&["sample", "drift"]));
        assert_eq!(normalize("sub-pixel shift"), s(&["sub", "pixel", "shift"]));
    }

    #[test]
    fn ranks_the_drift_viewer_for_a_tiff_stack_question() {
        let syn = Synonyms::new(&[
            s(&["tiff", "tif", "image", "images"]),
            s(&["stack", "series", "runs"]),
        ]);
        let hays = vec![
            Haystack::new(
                "Sample Drift Viewer",
                &s(&["drift", "timepix", "runs", "integrated", "slider"]),
                &[],
                "Load a series of Timepix runs and flip through their integrated images.",
            ),
            Haystack::new(
                "Outlier Removal",
                &s(&["outlier", "hot pixel", "filter"]),
                &[],
                "Remove outliers from TIFF images; load runs by number.",
            ),
            Haystack::new("CT Reconstruction", &s(&["tomography"]), &[], "Neutron CT."),
        ];
        let r = rank(
            "tiff integrated images over a stack of runs",
            hays.iter().enumerate(),
            &syn,
        );
        assert_eq!(r.len(), 2, "{r:?}");
        assert_eq!(r[0].index, 0, "{r:?}");
        assert_eq!(r[0].matched, s(&["tiff", "integrated", "images", "stack", "runs"]));
        assert!(r[0].score > r[1].score);
    }

    #[test]
    fn example_phrase_beats_scattered_description_hits() {
        let syn = Synonyms::default();
        let hays = vec![
            Haystack::new(
                "Generic Viewer",
                &[],
                &[],
                "Shows the sample; reports drift of the beam over time.",
            ),
            Haystack::new(
                "Sample Drift Viewer",
                &[],
                &s(&["did my sample move during the experiment"]),
                "",
            ),
        ];
        let r = rank("did my sample move", hays.iter().enumerate(), &syn);
        assert_eq!(r[0].index, 1, "{r:?}");
        assert_eq!(r[0].score, WEIGHT_NAME + WEIGHT_TAG + EXAMPLE_COVER_BONUS);
    }

    #[test]
    fn empty_or_stop_word_question_gives_nothing() {
        let hays = vec![Haystack::new("X", &[], &[], "the tool")];
        assert!(rank("", hays.iter().enumerate(), &Synonyms::default()).is_empty());
        assert!(rank("the a of", hays.iter().enumerate(), &Synonyms::default()).is_empty());
    }

    /// The real catalogue: the question that motivated the Ask box must
    /// land on the Sample Drift Viewer.
    #[test]
    fn real_catalogue_finds_the_sample_drift_viewer() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/applications.toml");
        let text = std::fs::read_to_string(path).unwrap();
        let cfg: toml::Value = toml::from_str(&text).unwrap();
        let groups: Vec<Vec<String>> = cfg
            .get("ask")
            .and_then(|a| a.get("synonyms"))
            .and_then(|s| s.clone().try_into().ok())
            .unwrap_or_default();
        let syn = Synonyms::new(&groups);
        let str_list = |v: Option<&toml::Value>| -> Vec<String> {
            v.and_then(|v| v.clone().try_into().ok()).unwrap_or_default()
        };
        let apps: Vec<(String, Haystack)> = cfg["app"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                let name = a["name"].as_str().unwrap().to_owned();
                let hay = Haystack::new(
                    &name,
                    &str_list(a.get("tags")),
                    &str_list(a.get("ask_examples")),
                    a.get("description").and_then(|d| d.as_str()).unwrap_or(""),
                );
                (name, hay)
            })
            .collect();
        let r = rank(
            "tiff integrated images over a stack of runs",
            apps.iter().enumerate().map(|(i, (_, h))| (i, h)),
            &syn,
        );
        let top: Vec<&str> = r.iter().take(3).map(|s| apps[s.index].0.as_str()).collect();
        assert_eq!(top.first().copied(), Some("Sample Drift Viewer"), "{top:?}");
    }
}
