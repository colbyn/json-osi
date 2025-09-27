// src/norm_ir.rs
//! Phase-specific normalization IR.
//!
//! Goal: build a compact, canonical tree from `inference::U` without descending into branches we’ll discard.
//! Then adapt to `ir::Ty` for lowering/codegen.

use crate::inference::U;
use crate::ir;

/// Canonical, compact shape after normalization policies are applied.
#[derive(Debug, Clone)]
pub enum NTy {
    Null,
    Bool,
    Integer { min: Option<i64>, max: Option<i64> },
    Number  { min: Option<f64>, max: Option<f64> },

    /// Strings after policy:
    /// - tiny enums kept in `enum_`
    /// - else possibly a grex pattern
    /// - `format_uri` passes the URI hint through
    String {
        enum_: Vec<String>,
        pattern: Option<String>,
        format_uri: bool,
    },

    ArrayList {
        item: Box<NTy>,
        min_items: Option<u32>,
        max_items: Option<u32>,
    },
    ArrayTuple {
        elems: Vec<NTy>,   // exact arity after decision
        min_items: u32,    // last required index + 1 (pads required by value)
        max_items: u32,    // == elems.len()
    },

    Object {
        /// Stable order for deterministic downstream behavior (sorted by name).
        fields: Vec<NField>,
    },

    /// X ∪ null collapsed into `Nullable(X)`
    Nullable(Box<NTy>),

    /// Keep unions that cannot be simplified away.
    OneOf(Vec<NTy>),
}

#[derive(Debug, Clone)]
pub struct NField {
    pub name: String,
    pub ty: NTy,
    pub required: bool, // present & non-null in all objects
}

// -------------------- builder: U -> NTy (pure) --------------------

/// Build the normalization IR from the evidence tree `U`.
/// - Decides tuple vs list BEFORE recursing into array columns.
/// - Applies numeric/string policies.
/// - Clones only what survives; does not mutate `U`.
/// 
/// Build the normalization IR by **consuming** `U`.
/// Moves evidence out of `U` to avoid cloning large maps/vectors.
/// Decides tuple-vs-list before descending; identical policies to `normalize_to_norm`.
pub fn normalize_to_norm_consume(u: U) -> NTy {
    if u.is_exact_null() {
        return NTy::Null;
    }

    let mut arms = Vec::<NTy>::new();

    // 1) Arrays first
    if let Some(arr) = u.arr {
        // decide cheaply from counts
        let is_tuple = crate::inference::decide_tuple(&arr);

        // always normalize pooled list hypothesis (consume its Box<U>)
        let item_norm = Box::new(normalize_to_norm_consume(*arr.item));

        if !is_tuple {
            arms.push(NTy::ArrayList {
                item: item_norm,
                min_items: Some(arr.len_min),
                max_items: Some(arr.len_max),
            });
        } else {
            // consume cols vector
            let elems: Vec<NTy> = arr
                .cols
                .into_iter()
                .map(normalize_to_norm_consume)
                .collect();

            let max_items = elems.len() as u32;
            let min_items = if arr.len_min == arr.len_max && arr.len_max > 0 {
                max_items
            } else {
                crate::inference::tuple_min_items_arr(&crate::inference::ArrC {
                    len_min: arr.len_min,
                    len_max: arr.len_max,
                    item: Box::new(U::default()), // unused by tuple min calc
                    cols: Vec::new(),             // unused
                    present: arr.present.clone(), // counts are needed
                    non_null: arr.non_null.clone(),
                    samples: arr.samples,
                })
            };

            arms.push(NTy::ArrayTuple { elems, min_items, max_items });
        }
    }

    // 2) Objects next
    if let Some(obj) = u.obj {
        // consume the BTreeMap by iterating it; push into Vec and sort
        let mut fields: Vec<NField> = Vec::with_capacity(obj.fields.len());
        for (name, field_c) in obj.fields {
            let required = field_c.non_null_in == obj.seen_objects;
            let ty = normalize_to_norm_consume(field_c.ty); // consume nested U
            fields.push(NField { name, ty, required });
        }
        fields.sort_by(|a, b| a.name.cmp(&b.name));
        arms.push(NTy::Object { fields });
    }

    // 3) Numbers
    if let Some(num) = u.num {
        let integerish = (num.saw_int || num.saw_uint)
            && !num.saw_float
            && num.min_f64.0.is_finite()
            && num.max_f64.0.is_finite()
            && num.min_f64.0.fract() == 0.0
            && num.max_f64.0.fract() == 0.0;

        if integerish {
            arms.push(NTy::Integer {
                min: Some(num.min_f64.0 as i64),
                max: Some(num.max_f64.0 as i64),
            });
        } else {
            arms.push(NTy::Number {
                min: if num.min_f64.0.is_finite() { Some(num.min_f64.0) } else { None },
                max: if num.max_f64.0.is_finite() { Some(num.max_f64.0) } else { None },
            });
        }
    }

    // 4) Strings
    if let Some(mut str_c) = u.str_ {
        // Tiny-enum only if flag is on AND samples look human-ish within limits.
        let tiny_enum = crate::inference::ENABLE_STRING_ENUMS
            && str_c.lits.len() <= crate::inference::STRING_ENUM_MAX
            && str_c.lits.iter().all(|s|
                s.len() <= crate::inference::STRING_ENUM_MAX_LEN
                && crate::inference::str::looks_humanish(s)
            );

        let (enum_, pattern) = if tiny_enum && !str_c.lits.is_empty() {
            // keep tiny enum
            let mut v: ::std::vec::Vec<::std::string::String> = str_c.lits.into_iter().collect();
            v.sort_unstable();
            (v, None)
        } else if !str_c.is_uri {
            // synthesize regex only if enabled; otherwise plain string
            let rx = if crate::inference::ENABLE_GREX {
                let key_now = crate::inference::str::grex_cache_key(&str_c.lits);
                if str_c.grex_cache_key == Some(key_now) {
                    str_c.pattern_synth.take()
                } else {
                    crate::inference::str::synth_regex_with_grex(&str_c.lits)
                }
            } else {
                None
            };
            // drop atoms either way to keep result compact
            str_c.lits.clear();
            (Vec::new(), rx)
        } else {
            // URI: plain string with format; drop atoms
            str_c.lits.clear();
            (Vec::new(), None)
        };

        arms.push(NTy::String {
            enum_,
            pattern,
            format_uri: str_c.is_uri,
        });
    }


    // 5) Bool
    if u.has_bool {
        arms.push(NTy::Bool);
    }

    // Assemble + collapse null
    let core = match arms.len() {
        0 => NTy::Null,
        1 => arms.into_iter().next().unwrap(),
        _ => simplify_norm_unions(arms),
    };

    if u.nullable && !matches!(core, NTy::Null) {
        NTy::Nullable(Box::new(core))
    } else {
        core
    }
}

fn simplify_norm_unions(mut arms: Vec<NTy>) -> NTy {
    let mut had_null = false;
    arms.retain(|t| {
        if matches!(t, NTy::Null) {
            had_null = true;
            false
        } else {
            true
        }
    });
    let core = match arms.len() {
        0 => NTy::Null,
        1 => arms.remove(0),
        _ => NTy::OneOf(arms),
    };
    if had_null {
        NTy::Nullable(Box::new(core))
    } else {
        core
    }
}

// -------------------- adapter: NTy -> ir::Ty --------------------

pub fn lower_from_norm(n: &NTy) -> ir::Ty {
    match n {
        NTy::Null => ir::Ty::Null,
        NTy::Bool => ir::Ty::Bool,

        NTy::Integer { min, max } => ir::Ty::Integer { min: *min, max: *max },
        NTy::Number  { min, max } => ir::Ty::Number  { min: *min, max: *max },

        NTy::String { enum_, pattern, format_uri } => ir::Ty::String {
            enum_: enum_.clone(),
            pattern: pattern.clone(),
            format_uri: *format_uri,
        },

        NTy::ArrayList { item, min_items, max_items } => ir::Ty::ArrayList {
            item: Box::new(lower_from_norm(item)),
            min_items: *min_items,
            max_items: *max_items,
        },

        NTy::ArrayTuple { elems, min_items, max_items } => ir::Ty::ArrayTuple {
            elems: elems.iter().map(lower_from_norm).collect(),
            min_items: *min_items,
            max_items: *max_items,
        },

        NTy::Object { fields } => ir::Ty::Object {
            fields: fields.iter().map(|f| ir::Field {
                name: f.name.clone(),
                ty: lower_from_norm(&f.ty),
                required: f.required,
            }).collect(),
        },

        NTy::Nullable(inner) => ir::Ty::Nullable(Box::new(lower_from_norm(inner))),
        NTy::OneOf(arms)     => ir::Ty::OneOf(arms.iter().map(lower_from_norm).collect()),
    }
}


// ————————————————————————————————————————————————————————————————————————————
// JSON SCHEMA CG
// ————————————————————————————————————————————————————————————————————————————

/// Build a JSON Schema (draft-ish) directly from the normalized IR.
/// This mirrors your existing schema semantics but uses the compact NTy.
pub fn schema_from_norm(n: &NTy) -> serde_json::Value {
    use serde_json::{json, Value};

    fn obj_of(props: Vec<(String, Value)>, required: Vec<String>) -> Value {
        let mut map = serde_json::Map::new();
        map.insert("type".into(), Value::from("object"));
        let mut props_map = serde_json::Map::new();
        for (k, v) in props {
            props_map.insert(k, v);
        }
        map.insert("properties".into(), Value::Object(props_map));
        if !required.is_empty() {
            map.insert(
                "required".into(),
                Value::Array(required.into_iter().map(Value::from).collect()),
            );
        }
        Value::Object(map)
    }

    fn nullable(inner: Value) -> Value {
        // Use oneOf to keep parity with your current emitter.
        json!({ "oneOf": [inner, { "type": "null" }] })
    }

    match n {
        NTy::Null => json!({ "type": "null" }),
        NTy::Bool => json!({ "type": "boolean" }),

        NTy::Integer { min, max } => {
            let mut o = json!({ "type": "integer" });
            if let Some(m) = *min { o["minimum"] = Value::from(m); }
            if let Some(m) = *max { o["maximum"] = Value::from(m); }
            o
        }

        NTy::Number { min, max } => {
            let mut o = json!({ "type": "number" });
            if let Some(m) = *min { o["minimum"] = Value::from(m); }
            if let Some(m) = *max { o["maximum"] = Value::from(m); }
            o
        }

        NTy::String { enum_, pattern, format_uri } => {
            let mut o = json!({ "type": "string" });
            if !enum_.is_empty() {
                o["enum"] = Value::Array(enum_.iter().cloned().map(Value::from).collect());
            } else if let Some(rx) = pattern {
                o["pattern"] = Value::from(rx.clone());
            }
            if *format_uri {
                o["format"] = Value::from("uri");
            }
            o
        }

        NTy::ArrayList { item, min_items, max_items } => {
            let mut o = json!({
                "type": "array",
                "items": schema_from_norm(item),
            });
            if let Some(mn) = *min_items { o["minItems"] = Value::from(mn); }
            if let Some(mx) = *max_items { o["maxItems"] = Value::from(mx); }
            o
        }

        NTy::ArrayTuple { elems, min_items, max_items } => {
            json!({
                "type": "array",
                "prefixItems": elems.iter().map(schema_from_norm).collect::<Vec<_>>(),
                "minItems": *min_items,
                "maxItems": *max_items
            })
        }

        NTy::Object { fields } => {
            let props = fields.iter()
                .map(|f| (f.name.clone(), schema_from_norm(&f.ty)))
                .collect::<Vec<_>>();
            let req = fields.iter()
                .filter(|f| f.required)
                .map(|f| f.name.clone())
                .collect::<Vec<_>>();
            obj_of(props, req)
        }

        NTy::Nullable(inner) => {
            let inner_schema = schema_from_norm(inner);
            // If the inner is exactly null (shouldn’t happen), return null;
            // otherwise wrap with oneOf [inner, null].
            if inner_schema == json!({"type": "null"}) {
                inner_schema
            } else {
                nullable(inner_schema)
            }
        }

        NTy::OneOf(arms) => {
            // Emit oneOf over child schemas; do not de-duplicate aggressively here
            // to keep behavior predictable. (Optional: collapse nested oneOfs.)
            json!({ "oneOf": arms.iter().map(schema_from_norm).collect::<Vec<_>>() })
        }
    }
}

/// Convenience: normalize `U` → NTy → JSON Schema
pub fn schema_from_u(u: crate::inference::U) -> serde_json::Value {
    let n = normalize_to_norm_consume(u);
    schema_from_norm(&n)
}


// -------------------- convenience (optional) --------------------

/// Pure normalization + lowering in one go.
pub fn normalize_and_lower(u: U) -> ir::Ty {
    let n = normalize_to_norm_consume(u);
    lower_from_norm(&n)
}

