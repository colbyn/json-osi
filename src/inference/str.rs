use std::collections::BTreeSet;

#[derive(Clone, Debug, Default)]
pub struct StrC {
    pub lits: BTreeSet<String>,
    // pub lcp: Option<String>,
    pub is_uri: bool,
    
    /// Regex synthesized during normalize (via grex). Prefer this over LCP.
    pub pattern_synth: Option<String>,

    /// Cache key for the last grex run: (distinct_count, total_chars, rolling_hash).
    /// We re-synthesize only when this key changes.
    pub grex_cache_key: Option<(usize, usize, u64)>,
}

// ------- Regex synthesis policy (grex integration) -------

/// Minimum distinct literals before we even consider synthesizing a regex.
const GREX_MIN_SAMPLES: usize = 3;

/// Hard cap on the length of a generated regex. If grex exceeds this,
/// we treat the field as an arbitrary string (no pattern).
const GREX_MAX_PATTERN_LEN: usize = 512;

/// Guard against regexes that are basically giant whitelists made of many
/// alternations. This is a coarse, top-level `|` count threshold.
const GREX_MAX_ALTS: usize = 64;


/// Compute a cheap, deterministic fingerprint of the current literal set.
/// We include the distinct count, total Unicode scalar count, and a rolling hash
/// over the sorted (BTreeSet) contents. If this changes, the set truly changed.
pub fn grex_cache_key(samples: &BTreeSet<String>) -> (usize, usize, u64) {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let count = samples.len();
    let total_chars = samples.iter().map(|s| s.chars().count()).sum::<usize>();

    let mut h = DefaultHasher::new();
    for s in samples {
        // NOTE: Keep this normalization exactly in sync with synth path (trim).
        // If you trim there, trim here too, so cache keys are consistent.
        let t = s.trim();
        t.hash(&mut h);
        // separator makes ["ab","c"] different from ["a","bc"]
        0xFFu8.hash(&mut h);
    }
    (count, total_chars, h.finish())
}

/// Very coarse “structure” guardrail: reject regexes with too many top-level '|'.
/// We don’t try to parse; this is just a cheap cutoff to avoid giant whitelists.
fn too_many_alternations(rx: &str) -> bool {
    rx.as_bytes().iter().filter(|&&b| b == b'|').count() > GREX_MAX_ALTS
}

/// Build an anchored regex with grex over the *full* literal set.
/// Minimal, safe path:
/// - Trim whitespace and drop empties to avoid learning "accept empty".
/// - Deterministic order (sort) for stable codegen.
/// - No prefix/anchor surgery: we take grex's anchored `^...$` as-is.
/// - Guardrails: drop result if too long or too alternation-heavy.
pub fn synth_regex_with_grex(samples: &BTreeSet<String>) -> Option<String> {
    use grex::RegExpBuilder;

    if samples.len() < GREX_MIN_SAMPLES {
        return None;
    }

    // Normalize exactly as your pipeline expects to validate; at minimum trim.
    let mut lits: Vec<&str> = samples
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    // After trimming, we may dip below the minimum.
    if lits.len() < GREX_MIN_SAMPLES {
        return None;
    }

    lits.sort_unstable();

    // grex 1.4.5: build() returns `^...$`.
    let rx = RegExpBuilder::from(&lits).build();

    if rx.len() > GREX_MAX_PATTERN_LEN || too_many_alternations(&rx) {
        return None; // fall back to enum/LCP/plain string
    }
    Some(rx)
}

pub fn join_str(a: &StrC, b: &StrC) -> StrC {
    let mut out = StrC::default();
    out.lits = &a.lits | &b.lits;
    if out.lits.len() > super::MAX_STR_LITS {
        out.lits.clear();
    }
    // out.lcp = lcp_join(a.lcp.as_deref(), b.lcp.as_deref());
    out.is_uri = a.is_uri && b.is_uri;
    out
}

fn lcp_join(a: Option<&str>, b: Option<&str>) -> Option<String> {
    match (a, b) {
        (Some(x), Some(y)) => {
            let mut out = String::new();
            for (cx, cy) in x.chars().zip(y.chars()) {
                if cx == cy { out.push(cx); } else { break; }
            }
            if out.is_empty() { None } else { Some(out) }
        }
        (Some(x), None) => Some(x.to_string()),
        (None, Some(y)) => Some(y.to_string()),
        (None, None) => None,
    }
}

pub fn lcp_set<'a, I>(mut it: I) -> Option<String> where I: Iterator<Item = &'a str> {
    let first = it.next()?;
    let mut acc = first.to_string();
    for s in it {
        acc = lcp_join(Some(&acc), Some(s)).unwrap_or_default();
        if acc.is_empty() { return Some(String::new()); }
    }
    Some(acc)
}

pub fn escape_regex(s: &str) -> String {
    // let mut out = String::with_capacity(s.len());
    // for c in s.chars() {
    //     match c {
    //         '.' | '+' | '*' | '?' | '^' | '$' | '(' | ')' | '[' | ']' |
    //         '{' | '}' | '|' | '\\' => { out.push('\\'); out.push(c); }
    //         _ => out.push(c),
    //     }
    // }
    // out
    regex::escape(s)
}

pub fn looks_like_uri(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
        || s.starts_with("mailto:") || s.starts_with("tel:")
}

pub fn looks_humanish(s: &str) -> bool {
    // lightweight: letters/digits/space/dash/underscore and not too long
    s.len() <= super::STRING_ENUM_MAX_LEN &&
    s.chars().all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-' || c == '_')
}