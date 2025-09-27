//! Minimal CNF/LUB inference engine (single-file).
//!
//! Stream JSON samples in, compute a least-upper-bound (LUB) schema using a
//! bounded set of constraints (one-per-kind arms), normalize to canonical
//! form (CNF), and optionally emit a compact JSON-ish schema description.
//!
//! Design goals:
//! - No permutation explosion; no history besides sufficient statistics.
//! - Join ⊔ is associative/commutative/idempotent → order-independent.
//! - Strings default to String (tokens → pattern); tiny human enums optional.
//! - Arrays keep tuple+list evidence together; finalization stays trivial.
pub mod str;
pub mod num;
pub mod obj;
pub mod arr;

use serde_json::{Map, Value};
use ordered_float::OrderedFloat;

pub use str::StrC;
pub use num::NumC;
pub use obj::{ObjC, FieldC};
pub use arr::ArrC;

// ------------------------------- Policy ---------------------------------- //


// const LCP_MIN_FOR_PATTERN: usize = 3;                 // promote to pattern if lcp ≥ this
pub const STRING_ENUM_MAX: usize = 8;                    // small, human-ish enum threshold
pub const STRING_ENUM_MAX_LEN: usize = 16;               // max literal length for enum
pub const KEEP_NUM_ATOMS_OUTSIDE_INTERVAL: bool = false; // simplest: widen

// literal caps to avoid ballooning before normalize prunes
pub const MAX_STR_LITS: usize = 64;
pub const MAX_NUM_LITS: usize = 64;

/// Feature flag: disable regex synthesis entirely (for testing memory/shape).
/// When false, no patterns are synthesized; non-enum, non-URI strings become plain strings.
pub const ENABLE_GREX: bool = false;

/// Feature flag: enable tiny, human-ish string enums inferred from literals.
/// When false, string enums are never emitted; strings become pattern (if enabled) or plain.
pub const ENABLE_STRING_ENUMS: bool = false;

/// Feature flags: control whether generated deserializers enforce numeric bounds.
/// These are codegen-time switches: change here, re-generate models, done.
pub const CHECK_INT_BOUNDS: bool = false;

/// Feature flags: control whether generated deserializers enforce numeric bounds.
/// These are codegen-time switches: change here, re-generate models, done.
/// f64 uses tolerant compare
pub const CHECK_NUM_BOUNDS: bool = false;


// ------------------------------ State (CNF) ------------------------------- //

#[derive(Clone, Debug, Default)]
pub struct U {
    pub nullable: bool,
    pub has_bool: bool,
    pub num: Option<NumC>,
    pub str_: Option<StrC>,
    pub arr: Option<ArrC>,
    pub obj: Option<ObjC>,
}

impl U {
    pub fn empty() -> Self { Self::default() }
    pub fn is_bottom(&self) -> bool {
        !self.nullable && !self.has_bool
            && self.num.is_none() && self.str_.is_none()
            && self.arr.is_none() && self.obj.is_none()
    }
    pub fn is_exact_null(&self) -> bool {
        self.nullable
            && !self.has_bool
            && self.num.is_none()
            && self.str_.is_none()
            && self.arr.is_none()
            && self.obj.is_none()
    }
}

// ------------------------------ Observe ---------------------------------- //

pub fn observe_value(v: &Value) -> U {
    match v {
        Value::Null => U { nullable: true, ..U::default() },
        Value::Bool(_) => U { has_bool: true, ..U::default() },
        Value::Number(n) => {
            let mut num = NumC::default();
            if let Some(i) = n.as_i64() {
                let f = OrderedFloat(i as f64);
                num.saw_int = true;
                num.lits_f64.insert(f);
                num.min_f64 = f;
                num.max_f64 = f;
            } else if let Some(u) = n.as_u64() {
                let f = OrderedFloat(u as f64);
                num.saw_uint = true;
                num.lits_f64.insert(f);
                num.min_f64 = f;
                num.max_f64 = f;
            } else if let Some(f) = n.as_f64() {
                let f = OrderedFloat(f);
                num.saw_float = true;
                num.lits_f64.insert(f);
                num.min_f64 = f;
                num.max_f64 = f;
            }
            U { num: Some(num), ..U::default() }
        }
        Value::String(s) => {
            let mut str_c = StrC::default();
            str_c.lits.insert(s.clone());
            // str_c.lcp = Some(s.clone());
            str_c.is_uri = str::looks_like_uri(s);
            U { str_: Some(str_c), ..U::default() }
        }
        Value::Array(xs) => observe_array(xs),
        Value::Object(m) => observe_object(m),
    }
}

// const TUPLEIZE_SMALL_HOMOGENEOUS_LIMIT: usize = 2;

fn observe_array(xs: &Vec<Value>) -> U {
    let mut arr = ArrC::default();
    arr.samples = 1;
    let len = xs.len() as u32;
    arr.len_min = len;
    arr.len_max = len;

    // list evidence
    let mut item = U::empty();
    for el in xs { item = U::join(&item, &observe_value(el)); }
    arr.item = Box::new(item);

    // tuple evidence + counts
    for (i, el) in xs.iter().enumerate() {
        if arr.cols.len() <= i {
            arr.cols.resize_with(i + 1, U::empty);
            arr.present.resize(i + 1, 0);
            arr.non_null.resize(i + 1, 0);
        }
        arr.cols[i] = U::join(&arr.cols[i], &observe_value(el));
        arr.present[i] += 1;
        if !matches!(el, Value::Null) { arr.non_null[i] += 1; }
    }

    U { arr: Some(arr), ..U::default() }
}

fn observe_object(map: &Map<String, Value>) -> U {
    let mut obj = ObjC::default();
    obj.seen_objects = 1;
    for (k, v) in map {
        let ty = observe_value(v);
        let non_null = !matches!(v, Value::Null);
        obj.fields.insert(k.clone(), FieldC {
            ty,
            present_in: 1,
            non_null_in: if non_null { 1 } else { 0 },
        });
    }
    U { obj: Some(obj), ..U::default() }
}

// -------------------------------- Join (⊔) -------------------------------- //

impl U {
    pub fn join(a: &Self, b: &Self) -> Self {
        let mut out = U::empty();

        out.nullable = a.nullable || b.nullable;
        out.has_bool = a.has_bool || b.has_bool;

        out.num = match (&a.num, &b.num) {
            (None, None) => None,
            (Some(x), None) | (None, Some(x)) => Some(x.clone()),
            (Some(x), Some(y)) => Some(NumC::join(x, y)),
        };

        out.str_ = match (&a.str_, &b.str_) {
            (None, None) => None,
            (Some(x), None) | (None, Some(x)) => Some(x.clone()),
            (Some(x), Some(y)) => Some(StrC::join(x, y)),
        };

        out.arr = match (&a.arr, &b.arr) {
            (None, None) => None,
            (Some(x), None) | (None, Some(x)) => Some(x.clone()),
            (Some(x), Some(y)) => Some(ArrC::join(x, y)),
        };

        out.obj = match (&a.obj, &b.obj) {
            (None, None) => None,
            (Some(x), None) | (None, Some(x)) => Some(x.clone()),
            (Some(x), Some(y)) => Some(ObjC::join(x, y)),
        };

        out
    }
}


// ------------------------------- Normalize -------------------------------- //

// /// Normalize in-place to a canonical shape (CNF).
// impl U {
//     pub fn normalize_mut(u: &mut Self) {
//         // Numbers: drop literals subsumed by interval (policy-controlled)
//         if let Some(num) = &mut u.num {
//             if num.min_f64.is_finite() && num.max_f64.is_finite() {
//                 num.lits_f64 = num.lits_f64
//                     .iter()
//                     .cloned()
//                     .filter(|x| {
//                         !(num.min_f64 <= *x && *x <= num.max_f64) && KEEP_NUM_ATOMS_OUTSIDE_INTERVAL
//                     })
//                     .collect();
//             }
//         }

//         // Strings: tiny human enums stay; otherwise synthesize regex via grex (URIs skip regex)
//         if let Some(str_c) = &mut u.str_ {
//             let tiny = {
//                 let check_1 = str_c.lits.len() <= STRING_ENUM_MAX;
//                 let check_2 = str_c.lits
//                     .iter()
//                     .all(|s| {
//                         s.len() <= STRING_ENUM_MAX_LEN && str::looks_humanish(s)
//                     });
//                 check_1 && check_2
//             };

//             if !tiny {
//                 if !str_c.is_uri {
//                     // Attempt grex
//                     let key_now = str::grex_cache_key(&str_c.lits);
//                     if str_c.grex_cache_key != Some(key_now) {
//                         str_c.pattern_synth = str::synth_regex_with_grex(&str_c.lits);
//                         str_c.grex_cache_key = Some(key_now);
//                     }
//                     // Collapse to plain string if grex didn’t produce a pattern
//                     str_c.lits.clear(); // <-- move this out of the `if pattern_synth.is_some()` branch
//                 } else {
//                     str_c.lits.clear();
//                 }
//             }
//         }

//         // Arrays: recurse; tuple vs list is a codegen/schema decision
//         if let Some(arr) = &mut u.arr {
//             U::normalize_mut(&mut arr.item);
//             for c in &mut arr.cols { U::normalize_mut(c); }

//             // Decide tuple vs list
//             if !decide_tuple(arr) {
//                 // collapse to list: drop positional evidence
//                 arr.cols.clear();
//                 arr.present.clear();
//                 arr.non_null.clear();
//             }

//             // (len_min/len_max stay as observed; codegen can choose to enforce them.)
//         }


//         // Objects: recurse
//         if let Some(obj) = &mut u.obj {
//             for f in obj.fields.values_mut() {
//                 U::normalize_mut(&mut f.ty);
//             }
//         }
//         // Note: no arm-flattening needed—U already enforces ≤1 per kind.
//     }
// }

/// New, conservative normalizer (v2): decide tuple vs list *before* recursing.
/// Semantics are identical to v1, but it avoids deep work on columns that
/// will be discarded when the array is a list.
///
/// Keep v1 (`U::normalize_mut`) for comparison.
pub fn normalize2_mut(u: &mut U) {
    // ---- Numbers: same policy as v1 ----
    if let Some(num) = &mut u.num {
        if num.min_f64.is_finite() && num.max_f64.is_finite() {
            num.lits_f64 = num
                .lits_f64
                .iter()
                .cloned()
                .filter(|x| {
                    !(num.min_f64 <= *x && *x <= num.max_f64) && KEEP_NUM_ATOMS_OUTSIDE_INTERVAL
                })
                .collect();
        }
    }

    // ---- Strings: tiny enum (flagged) else pattern (flagged) / URI / plain ----
    if let Some(str_c) = &mut u.str_ {
        let tiny = crate::inference::ENABLE_STRING_ENUMS
            && str_c.lits.len() <= STRING_ENUM_MAX
            && str_c
                .lits
                .iter()
                .all(|s| s.len() <= STRING_ENUM_MAX_LEN && crate::inference::str::looks_humanish(s));

        if !tiny {
            if !str_c.is_uri {
                if crate::inference::ENABLE_GREX {
                    let key_now = crate::inference::str::grex_cache_key(&str_c.lits);
                    if str_c.grex_cache_key != Some(key_now) {
                        str_c.pattern_synth = crate::inference::str::synth_regex_with_grex(&str_c.lits);
                        str_c.grex_cache_key = Some(key_now);
                    }
                } else {
                    str_c.pattern_synth = None;
                }
                str_c.lits.clear(); // collapse atoms regardless
            } else {
                str_c.lits.clear();
            }
        }
    }

    // ---- Arrays: DECIDE FIRST, then recurse accordingly ----
    if let Some(arr) = &mut u.arr {
        // Decide tuple vs list using *only* counts/lengths (cheap).
        let is_tuple = decide_tuple(arr);

        // Always normalize the pooled list hypothesis.
        normalize2_mut(&mut arr.item);

        if !is_tuple {
            // Collapse early: skip normalizing per-position children entirely.
            arr.cols.clear();
            arr.present.clear();
            arr.non_null.clear();
        } else {
            for c in &mut arr.cols {
                normalize2_mut(c);
            }
            // keep len_min/len_max as observed; lower/codegen enforce tuple shape
        }
    }

    // ---- Objects: recurse into fields (same as v1) ----
    if let Some(obj) = &mut u.obj {
        for f in obj.fields.values_mut() {
            normalize2_mut(&mut f.ty);
        }
    }
    // Union flattening not needed here; done in lowering.
}


/// Return true if we have *proof* this is a tuple:
///  - exact arity (all arrays same length), or
///  - at least one position is an exact-null pad across all samples.
pub fn decide_tuple(arr: &ArrC) -> bool {
    if arr.samples < 2 { return false; }
    if arr.cols.is_empty() { return false; }

    // Proof 1: every observed array had the same length
    if arr.len_min == arr.len_max && arr.len_max > 0 {
        return true;
    }

    // Proof 2: exact-null pad in some column
    let n = arr.cols.len();
    for i in 0..n {
        let present  = *arr.present.get(i).unwrap_or(&0);
        let non_null = *arr.non_null.get(i).unwrap_or(&0);
        if present == arr.samples && non_null == 0 {
            return true;
        }
    }

    // Otherwise, we have insufficient evidence → treat as homogeneous list.
    false
}

// ------------------------------- Utilities -------------------------------- //


pub fn tuple_min_items_arr(arr: &ArrC) -> u32 {
    let mut last_req: i32 = -1;
    for i in 0..arr.cols.len() {
        let present = *arr.present.get(i).unwrap_or(&0);
        if present == arr.samples {
            last_req = i as i32;
        }
    }
    if last_req < 0 { 0 } else { (last_req as u32) + 1 }
}


// ------------------------------- Tests ------------------------------------ //

