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

use std::collections::BTreeMap;
use serde_json::{Map, Value};
use ordered_float::OrderedFloat;

pub use str::StrC;
pub use num::NumC;

// ------------------------------- Policy ---------------------------------- //

// const LCP_MIN_FOR_PATTERN: usize = 3;      // promote to pattern if lcp ≥ this
const STRING_ENUM_MAX: usize = 8;          // small, human-ish enum threshold
const STRING_ENUM_MAX_LEN: usize = 16;      // max literal length for enum
const KEEP_NUM_ATOMS_OUTSIDE_INTERVAL: bool = false; // simplest: widen

// literal caps to avoid ballooning before normalize prunes
const MAX_STR_LITS: usize = 64;
const MAX_NUM_LITS: usize = 64;

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

#[derive(Clone, Debug, Default)]
pub struct ArrC {
    pub len_min: u32,
    pub len_max: u32,
    pub item: Box<U>,          // list hypothesis
    pub cols: Vec<U>,          // tuple hypothesis, per position
    pub present: Vec<u64>,     // how many arrays had a value (incl. null) at pos i
    pub non_null: Vec<u64>,    // how many arrays had a non-null value at pos i
    pub samples: u64,          // arrays observed for this slot
}


#[derive(Clone, Debug, Default)]
pub struct ObjC {
    pub fields: BTreeMap<String, FieldC>,
    pub seen_objects: u64,
}

#[derive(Clone, Debug, Default)]
pub struct FieldC {
    pub ty: U,
    pub present_in: u64,
    pub non_null_in: u64, // for "required" = present & non-null
}

impl U {
    pub fn empty() -> Self { Self::default() }
    pub fn is_bottom(&self) -> bool {
        !self.nullable && !self.has_bool
            && self.num.is_none() && self.str_.is_none()
            && self.arr.is_none() && self.obj.is_none()
    }
}

// /// kind enum + detector
// #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
// enum K { Null, Bool, Num, Str, Arr, Obj }

// fn kind_of(v: &Value) -> K {
//     match v {
//         Value::Null      => K::Null,
//         Value::Bool(_)   => K::Bool,
//         Value::Number(_) => K::Num,
//         Value::String(_) => K::Str,
//         Value::Array(_)  => K::Arr,
//         Value::Object(_) => K::Obj,
//     }
// }


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
    for el in xs { item = join(&item, &observe_value(el)); }
    arr.item = Box::new(item);

    // tuple evidence + counts
    for (i, el) in xs.iter().enumerate() {
        if arr.cols.len() <= i {
            arr.cols.resize_with(i + 1, U::empty);
            arr.present.resize(i + 1, 0);
            arr.non_null.resize(i + 1, 0);
        }
        arr.cols[i] = join(&arr.cols[i], &observe_value(el));
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

pub fn join(a: &U, b: &U) -> U {
    let mut out = U::empty();

    out.nullable = a.nullable || b.nullable;
    out.has_bool = a.has_bool || b.has_bool;

    out.num = match (&a.num, &b.num) {
        (None, None) => None,
        (Some(x), None) | (None, Some(x)) => Some(x.clone()),
        (Some(x), Some(y)) => Some(num::join_num(x, y)),
    };

    out.str_ = match (&a.str_, &b.str_) {
        (None, None) => None,
        (Some(x), None) | (None, Some(x)) => Some(x.clone()),
        (Some(x), Some(y)) => Some(str::join_str(x, y)),
    };

    out.arr = match (&a.arr, &b.arr) {
        (None, None) => None,
        (Some(x), None) | (None, Some(x)) => Some(x.clone()),
        (Some(x), Some(y)) => Some(join_arr(x, y)),
    };

    out.obj = match (&a.obj, &b.obj) {
        (None, None) => None,
        (Some(x), None) | (None, Some(x)) => Some(x.clone()),
        (Some(x), Some(y)) => Some(join_obj(x, y)),
    };

    out
}

// fn missing_nullable() -> U { let mut u = U::empty(); u.nullable = true; u }

fn missing_nullable() -> U { let mut u = U::empty(); u.nullable = true; u }

fn join_arr(a: &ArrC, b: &ArrC) -> ArrC {
    let mut out = ArrC::default();
    out.len_min = a.len_min.min(b.len_min);
    out.len_max = a.len_max.max(b.len_max);
    out.samples = a.samples + b.samples;
    out.item = Box::new(join(&a.item, &b.item));

    let n = a.cols.len().max(b.cols.len());
    out.cols = (0..n).map(|i| {
        let ai = a.cols.get(i).cloned().unwrap_or_else(missing_nullable);
        let bi = b.cols.get(i).cloned().unwrap_or_else(missing_nullable);
        join(&ai, &bi)
    }).collect();

    out.present = (0..n).map(|i| {
        a.present.get(i).copied().unwrap_or(0) + b.present.get(i).copied().unwrap_or(0)
    }).collect();

    out.non_null = (0..n).map(|i| {
        a.non_null.get(i).copied().unwrap_or(0) + b.non_null.get(i).copied().unwrap_or(0)
    }).collect();

    out
}

fn join_obj(a: &ObjC, b: &ObjC) -> ObjC {
    let mut out = ObjC::default();
    out.seen_objects = a.seen_objects + b.seen_objects;

    // merge keys from a
    for (k, fa) in &a.fields {
        match b.fields.get(k) {
            None => {
                out.fields.insert(k.clone(), FieldC {
                    ty: fa.ty.clone(),
                    present_in: fa.present_in,
                    non_null_in: fa.non_null_in,
                });
            }
            Some(fb) => {
                out.fields.insert(k.clone(), FieldC {
                    ty: join(&fa.ty, &fb.ty),
                    present_in: fa.present_in + fb.present_in,
                    non_null_in: fa.non_null_in + fb.non_null_in,
                });
            }
        }
    }
    // add keys only in b
    for (k, fb) in &b.fields {
        if !out.fields.contains_key(k) {
            out.fields.insert(k.clone(), FieldC {
                ty: fb.ty.clone(),
                present_in: fb.present_in,
                non_null_in: fb.non_null_in,
            });
        }
    }

    out
}

// ------------------------------- Normalize -------------------------------- //

/// Normalize in-place to a canonical shape (CNF).
pub fn normalize(u: &mut U) {
    // Numbers: drop literals subsumed by interval (policy-controlled)
    if let Some(num) = &mut u.num {
        if num.min_f64.is_finite() && num.max_f64.is_finite() {
            num.lits_f64 = num.lits_f64.iter()
                .cloned()
                .filter(|x| !(num.min_f64 <= *x && *x <= num.max_f64) && KEEP_NUM_ATOMS_OUTSIDE_INTERVAL)
                .collect();
        }
    }

    // Strings: tiny human enums stay; otherwise synthesize regex via grex (URIs skip regex)
    if let Some(str_c) = &mut u.str_ {
        let tiny = str_c.lits.len() <= STRING_ENUM_MAX
            && str_c.lits.iter().all(|s| s.len() <= STRING_ENUM_MAX_LEN && str::looks_humanish(s));

        if !tiny {
            if !str_c.is_uri {
                // Attempt grex
                let key_now = str::grex_cache_key(&str_c.lits);
                if str_c.grex_cache_key != Some(key_now) {
                    str_c.pattern_synth = str::synth_regex_with_grex(&str_c.lits);
                    str_c.grex_cache_key = Some(key_now);
                }
                // Collapse to plain string if grex didn’t produce a pattern
                str_c.lits.clear(); // <-- move this out of the `if pattern_synth.is_some()` branch
            } else {
                str_c.lits.clear();
            }
        }
    }

    // if let Some(str_c) = &mut u.str_ {
    //     // Recompute LCP from all literals for robustness (used only as a fallback)
    //     // str_c.lcp = str::lcp_set(str_c.lits.iter().map(|s| s.as_str()));

    //     if !str_c.lits.is_empty() {
    //         // Keep small human-ish enums; they’re clearer and safer than regexes.
    //         let tiny = str_c.lits.len() <= STRING_ENUM_MAX
    //             && str_c.lits.iter().all(|s| s.len() <= STRING_ENUM_MAX_LEN && str::looks_humanish(s));

    //         if !tiny {
    //             if !str_c.is_uri {
    //                 // Only synthesize when the literal set actually changed.
    //                 let key_now = str::grex_cache_key(&str_c.lits);
    //                 if str_c.grex_cache_key != Some(key_now) {
    //                     str_c.pattern_synth = str::synth_regex_with_grex(&str_c.lits);
    //                     str_c.grex_cache_key = Some(key_now);
    //                 }
    //                 // If we successfully learned a bounded regex, drop the enum literals.
    //                 if str_c.pattern_synth.is_some() {
    //                     str_c.lits.clear();
    //                 }
    //             } else {
    //                 // URIs → prefer `format:"uri"`, so don't keep bulky enums.
    //                 str_c.lits.clear();
    //             }
    //         }
    //         // If tiny enum, keep as-is; lowering will emit the enum.
    //     }
    // }



    // Arrays: recurse; tuple vs list is a codegen/schema decision
    if let Some(arr) = &mut u.arr {
        normalize(&mut arr.item);
        for c in &mut arr.cols { normalize(c); }

        // Decide tuple vs list
        if !decide_tuple(arr) {
            // collapse to list: drop positional evidence
            arr.cols.clear();
            arr.present.clear();
            arr.non_null.clear();
        }

        // (len_min/len_max stay as observed; codegen can choose to enforce them.)
    }


    // Objects: recurse
    if let Some(obj) = &mut u.obj {
        for f in obj.fields.values_mut() {
            normalize(&mut f.ty);
        }
    }
    // Note: no arm-flattening needed—U already enforces ≤1 per kind.
}

/// Return true if we have *proof* this is a tuple:
///  - exact arity (all arrays same length), or
///  - at least one position is an exact-null pad across all samples.
fn decide_tuple(arr: &ArrC) -> bool {
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



// ------------------------------- Emission --------------------------------- //

/// Minimal JSON Schema–ish emission (optional).
/// Uses JSON Schema tuple form via `prefixItems`.
pub fn emit_schema(u: &U) -> serde_json::Value {
    use serde_json::{json, Value};
    let mut arms: Vec<Value> = Vec::new();

    // ---- number arm ----
    if let Some(num) = &u.num {
        // Decide integer vs number
        let is_integerish = (num.saw_int || num.saw_uint) && !num.saw_float
            && num.min_f64.0.is_finite() && num.max_f64.0.is_finite()      // <-- use max here
            && num.min_f64.0.fract() == 0.0 && num.max_f64.0.fract() == 0.0; // <-- and here

        let mut o = json!({ "type": if is_integerish { "integer" } else { "number" } });

        if num.min_f64.0.is_finite() {
            o["minimum"] = json_num_pref_i64(num.min_f64.0);
        }
        if num.max_f64.0.is_finite() {  // <-- use max here
            o["maximum"] = json_num_pref_i64(num.max_f64.0);
        }

        if !num.lits_f64.is_empty() {
            o["enum_numbers"] = Value::Array(
                num.lits_f64.iter().map(|x| json_num_pref_i64(x.0)).collect()
            );
        }
        arms.push(o);
    }

    // ---- string arm ----
    if let Some(str_c) = &u.str_ {
        let mut o = serde_json::json!({ "type": "string" });

        if !str_c.lits.is_empty() {
            // Prefer enum when tiny; (normalize already prunes non-tiny)
            o["enum"] = serde_json::Value::Array(
                str_c.lits.iter().cloned().map(serde_json::Value::from).collect()
            );
        } else if let Some(rx) = &str_c.pattern_synth {
            // Prefer grex-synth pattern when present (already anchored).
            o["pattern"] = serde_json::Value::from(rx.clone());
        }
        
        // else if let Some(lcp) = &str_c.lcp {
        //     // Fallback: LCP with restricted tail (never naked dot-star)
        //     if lcp.chars().count() >= LCP_MIN_FOR_PATTERN {
        //         // Limit by character count to avoid slicing mid–codepoint
        //         let lcp_short: String = lcp.chars().take(80).collect();
        //         o["pattern"] = serde_json::Value::from(
        //             format!("^{}[A-Za-z0-9._%+\\-]*$", str::escape_regex(&lcp_short))
        //         );
        //     }
        // }

        if str_c.is_uri {
            o["format"] = serde_json::Value::from("uri");
        }
        arms.push(o);
    }

    // ---- boolean arm ----
    if u.has_bool {
        arms.push(json!({ "type": "boolean" }));
    }

    // ---- array arm ----
    if let Some(arr) = &u.arr {
        if !arr.cols.is_empty() {
            // tuple via prefixItems — tuple-aware min/max
            let max_items = arr.cols.len() as u32;
            let min_items = if arr.len_min == arr.len_max && arr.len_max > 0 {
                max_items
            } else {
                tuple_min_items_arr(arr)
            };

            arms.push(serde_json::json!({
                "type": "array",
                "prefixItems": arr.cols.iter().map(emit_schema).collect::<Vec<_>>(),
                "minItems": min_items,
                "maxItems": max_items
            }));

        } else {
            // homogeneous list — keep list-wise min/max
            arms.push(serde_json::json!({
                "type": "array",
                "items": emit_schema(&arr.item),
                "minItems": arr.len_min,
                "maxItems": arr.len_max
            }));
        }
    }


    // ---- object arm ----
    if let Some(obj) = &u.obj {
        let mut props = serde_json::Map::new();
        let mut required: Vec<String> = Vec::new();
        for (k, f) in &obj.fields {
            props.insert(k.clone(), emit_schema(&f.ty));
            // "required" means present & non-null in all objects
            if f.non_null_in == obj.seen_objects {
                required.push(k.clone());
            }
        }
        let mut o = json!({ "type": "object", "properties": props });
        if !required.is_empty() {
            o["required"] = Value::Array(required.into_iter().map(Value::from).collect());
        }
        arms.push(o);
    }

    // ---- assemble ----
    let core = match arms.len() {
        0 => {
            // No arms at all
            if u.nullable {
                serde_json::json!({ "type": "null" })
            } else {
                serde_json::json!({ "type": "any" })
            }
        }
        1 => arms.remove(0),
        _ => serde_json::json!({ "oneOf": arms }),
    };

    if u.nullable && core != serde_json::json!({"type":"null"}) {
        serde_json::json!({ "oneOf": [core, { "type": "null" }] })
    } else {
        core
    }
}

// Helper: prefer emitting integers when exact
fn json_num_pref_i64(n: f64) -> serde_json::Value {
    if n.is_finite() && n.fract() == 0.0 && n >= i64::MIN as f64 && n <= i64::MAX as f64 {
        serde_json::Value::from(n as i64)
    } else {
        serde_json::Value::from(n)
    }
}


// ------------------------------- Front API -------------------------------- //

pub struct Inference { state: U }

impl Inference {
    pub fn new() -> Self { Self { state: U::empty() } }

    pub fn observe_value(&mut self, v: &Value) {
        let obs = observe_value(v);
        self.state = join(&self.state, &obs);
    }

    pub fn solve(&self) -> U {
        let mut u = self.state.clone();
        normalize(&mut u);
        u
    }
}

pub fn infer_from_values<'a, I>(values: I) -> U
where
    I: IntoIterator<Item = &'a Value>
{
    let mut st = U::empty();
    for v in values {
        st = join(&st, &observe_value(v));
    }
    normalize(&mut st);
    st
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_interval_subsumes_literals() {
        let a = serde_json::json!(1.0);
        let b = serde_json::json!(5.0);
        let c = serde_json::json!(3.0);
        let u = infer_from_values([&a, &b, &c]);
        let num = u.num.unwrap();
        assert!(num.lits_f64.is_empty());
        assert_eq!(num.min_f64, 1.0);
        assert_eq!(num.max_f64, 5.0);
        assert!(num.saw_float || num.saw_int || num.saw_uint);
    }

    #[test]
    fn integers_emit_integer_type() {
        let a = serde_json::json!(1);
        let b = serde_json::json!(5);
        let u = infer_from_values([&a, &b]);
        let schema = emit_schema(&u);
        let ty = schema_pointer(&schema, "/oneOf/0/type").or_else(|| schema_pointer(&schema, "/type"));
        assert_eq!(ty.as_deref(), Some("integer"));
    }

    #[test]
    fn tiny_human_enum_stays_enum() {
        let s1 = Value::String("on".into());
        let s2 = Value::String("off".into());
        let u = infer_from_values([&s1, &s2]);
        let str_c = u.str_.unwrap();
        assert_eq!(str_c.lits.len(), 2);
    }

    #[test]
    fn objects_merge_and_requiredness_non_null() {
        let a = serde_json::json!({"x": 1, "y": "a"});
        let b = serde_json::json!({"x": 2, "y": null});
        let u = infer_from_values([&a, &b]);
        let obj = u.obj.unwrap();
        assert_eq!(obj.seen_objects, 2);
        let y = obj.fields.get("y").unwrap();
        assert_eq!(y.present_in, 2);
        // y is not "required" because null appeared once → non_null_in == 1
        assert_eq!(y.non_null_in, 1);
        let schema = emit_schema(&U { obj: Some(obj), ..U::default() });
        // required should only contain "x"
        let req = schema_pointer(&schema, "/oneOf/0/required").or_else(|| schema_pointer(&schema, "/required"));
        assert!(req.as_deref().unwrap_or("").contains("x"));
    }

    #[test]
    fn join_laws_idempotent_commutative_associative() {
        let a = serde_json::json!([1, "a"]);
        let b = serde_json::json!([2, "b"]);
        let c = serde_json::json!([3, "c"]);

        let ua = infer_from_values([&a]);
        let ub = infer_from_values([&b]);
        let uc = infer_from_values([&c]);

        // idempotent
        let aa = join(&ua, &ua);
        assert_eq!(serde_json::to_string(&emit_schema(&ua)).unwrap(),
                   serde_json::to_string(&emit_schema(&aa)).unwrap());

        // commutative
        let ab = join(&ua, &ub);
        let ba = join(&ub, &ua);
        assert_eq!(serde_json::to_string(&emit_schema(&ab)).unwrap(),
                   serde_json::to_string(&emit_schema(&ba)).unwrap());

        // associative
        let ab_c = join(&ab, &uc);
        let a_bc = join(&ua, &join(&ub, &uc));
        assert_eq!(serde_json::to_string(&emit_schema(&ab_c)).unwrap(),
                   serde_json::to_string(&emit_schema(&a_bc)).unwrap());
    }

    // tiny helper to read shallow JSON Pointers for tests
    fn schema_pointer<'a>(v: &'a Value, ptr: &str) -> Option<String> {
        v.pointer(ptr).and_then(|x| {
            if let Some(s) = x.as_str() { Some(s.to_string()) }
            else if x.is_array() || x.is_object() { Some(x.to_string()) }
            else { None }
        })
    }

    // #[test]
    // fn nested_lat_lon_bounds_are_per_position() {
    //     use serde_json::json;
    //     let samples = vec![
    //         json!(["id1", "A", [null, [37.4219, -122.0840], null], null, 4.1, true,  ["tools"], null, null]),
    //         json!(["id2", "B", [null, [36.9999, -121.9999], null], null, 4.5, null,  ["store"], null, null]),
    //     ];
    //     let u = infer_from_values(samples.iter().collect::<Vec<_>>().iter().map(|v| *v));
    //     let arr = u.arr.as_ref().expect("root tuple");
    //     let loc = arr.cols[2].arr.as_ref().expect("outer triple");
    //     assert_eq!(loc.cols.len(), 3);

    //     let inner = loc.cols[1].arr.as_ref().expect("inner pair");
    //     assert_eq!(inner.cols.len(), 2);

    //     let lat = inner.cols[0].num.as_ref().expect("lat");
    //     let lon = inner.cols[1].num.as_ref().expect("lon");

    //     assert!((lat.min_f64.0 - 36.9999).abs() < 1e-9);
    //     assert!((lat.max_f64.0 - 37.4219).abs() < 1e-9);
    //     assert!((lon.min_f64.0 + 122.0840).abs() < 1e-9); // -122.0840
    //     assert!((lon.max_f64.0 + 121.9999).abs() < 1e-9); // -121.9999
    // }

    #[test]
    fn nested_lat_lon_bounds_are_per_position() {
        use serde_json::json;

        // your current fixture
        let samples = vec![
            json!(["0ahUKEa1ZQ", "Acme Widgets",        [null, [37.4219, -122.0840], null], "https://example.com/a", 4.3,  true,  ["hardware","store"],  null, null]),
            json!(["0ahUKEa2ZQ", "Acme Widgets - East", [null, [37.4200, -122.0830], null], null,                    4.5,  null,  ["hardware"],          null, null]),
            json!(["0ahUKEa3ZQ", null,                  [null, [37.4225, -122.0855], null], "https://example.com/c", null, false, [],                    null, null]),
            json!(["0ahUKEa4ZQ", "ACME",                null,                               null,                    4,    null,  ["store","tools"],     null, null]),
            json!(["0ahUKEa5ZQ", "Acme West",           [null, [37.0000, -122.0000], null], "https://example.com/w", 4.1,  true,  ["hardware","outlet"], null, null]),
            json!(["0ahUKEa6ZQ", "Acme Central",        [null, null, null],                 null,                    null, null,  ["tools"],             null, null]),
        ];

        // infer
        let u = infer_from_values(samples.iter().collect::<Vec<_>>());

        // root must be a tuple (prefixItems)
        let root_arr = u.arr.as_ref().expect("root is array");
        assert!(!root_arr.cols.is_empty(), "root must have tuple cols");
        assert_eq!(root_arr.cols.len(), 9, "expect 9-tuple at root");

        // slot 2: [ null, [lat,lon], null ]
        let loc_outer = root_arr.cols[2].arr.as_ref().expect("outer triple");
        assert_eq!(loc_outer.cols.len(), 3, "outer triple has 3 slots");

        // middle slot is the inner [lat, lon] pair
        let inner = loc_outer.cols[1].arr.as_ref().expect("inner pair");
        assert_eq!(inner.cols.len(), 2, "inner pair has 2 slots (lat,lon)");

        let lat = inner.cols[0].num.as_ref().expect("lat is number");
        let lon = inner.cols[1].num.as_ref().expect("lon is number");

        // precise per-position bounds
        let eps = 1e-9;
        assert!((lat.min_f64.0 - 37.0000).abs() < eps, "lat min");
        assert!((lat.max_f64.0 - 37.4225).abs() < eps, "lat max");
        assert!((lon.min_f64.0 + 122.0855).abs() < eps, "lon min"); // -122.0855
        assert!((lon.max_f64.0 + 122.0000).abs() < eps, "lon max"); // -122.0000

        // exact-null padders are recognized, so outer triple has minItems == 3
        // (this assumes your tuple_min_items uses is_exact_null as discussed)
        let schema = emit_schema(&u);
        let outer_schema = &schema["prefixItems"][2]["oneOf"][0]; // array branch
        assert_eq!(outer_schema["minItems"], 3);
        assert_eq!(outer_schema["maxItems"], 3);
    }

    #[test]
    fn arrays_tuple_decision_and_min_items_rules() {
        use serde_json::json;
        // A) Variable length, no pad => list
        let a = json!([[1,"x"], [2]]);
        let ua = crate::inference::infer_from_values(a.as_array().unwrap().iter());
        {
            let arr = ua.arr.as_ref().unwrap();
            assert!(arr.cols.is_empty(), "variable length collapses to list");
        }

        // B) Exact-null pad => tuple with optional tail (min < max)
        let b = json!([[1, null, "x"], [2, null]]);
        let ub = crate::inference::infer_from_values(b.as_array().unwrap().iter());
        {
            let arr = ub.arr.as_ref().unwrap();
            assert!(!arr.cols.is_empty(), "kept as tuple due to hard null pad");
            let schema = crate::inference::emit_schema(&ub);
            let tuple = schema.get("prefixItems").unwrap(); // root is array only
            let _ = tuple; // TODO: SHOULD THIS BE READ?
            assert!(schema["minItems"].as_u64().unwrap() < schema["maxItems"].as_u64().unwrap());
        }

        // C) Exact arity => exact tuple (min == max)
        let c = json!([[1, "x", null], [3, "y", null]]);
        let uc = crate::inference::infer_from_values(c.as_array().unwrap().iter());
        {
            let arr = uc.arr.as_ref().unwrap();
            assert_eq!(arr.len_min, arr.len_max, "exact arity proven");
            let schema = crate::inference::emit_schema(&uc);
            assert_eq!(schema["minItems"], schema["maxItems"], "fixed length enforced");
        }
    }

    #[test]
    fn grex_prefix_truncation_survives_utf8() {
        use std::collections::BTreeSet;
        let mut set = BTreeSet::new();
        set.insert("näïve-αβγ-001".to_string());
        set.insert("näïve-αβγ-002".to_string());
        set.insert("näïve-αβγ-003".to_string());
        // Prior to the fix this could panic inside trim_common_prefix → truncate.
        let _rx = str::synth_regex_with_grex(&set);
    }

    #[test]
    fn schema_lcp_truncation_survives_utf8() {
        let a = serde_json::Value::String("https://例え.テスト/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into());
        let b = serde_json::Value::String("https://例え.テスト/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab".into());
        let u = super::infer_from_values([&a, &b]);
        let _schema = super::emit_schema(&u); // should not panic
    }
}


