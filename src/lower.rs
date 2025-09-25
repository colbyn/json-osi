use crate::inference::{tuple_min_items_arr, U};
use crate::ir::{Ty, Field};

fn is_exact_null(u: &crate::inference::U) -> bool {
    u.nullable
        && !u.has_bool
        && u.num.is_none()
        && u.str_.is_none()
        && u.arr.is_none()
        && u.obj.is_none()
}


// Utility: gate tiny prefix patterns
// const LCP_MIN_FOR_PATTERN: usize = 3;

pub fn lower_to_ir(u: &U) -> Ty {
    if is_exact_null(u) {
        // exactly null → emit Null (not Option<Null>)
        return Ty::Null;
    }
    let base = lower_core(u);
    if u.nullable { Ty::Nullable(Box::new(base)) } else { base }
}

fn lower_core(u: &U) -> Ty {
    let mut arms: Vec<Ty> = Vec::new();

    // 1) Arrays first (so when mixed with scalars, we try arrays before i64/f64)
    if let Some(arr) = &u.arr {
        if !arr.cols.is_empty() {
            let elems = arr.cols.iter().map(lower_to_ir).collect::<Vec<_>>();
            let max_items = arr.cols.len() as u32;
            let min_items = if arr.len_min == arr.len_max && arr.len_max > 0 {
                max_items
            } else {
                tuple_min_items_arr(arr)
            };
            arms.push(Ty::ArrayTuple { elems, min_items, max_items });
        } else {
            arms.push(Ty::ArrayList {
                item: Box::new(lower_to_ir(&arr.item)),
                min_items: Some(arr.len_min),
                max_items: Some(arr.len_max),
            });
        }
    }

    // 2) Objects next
    if let Some(obj) = &u.obj {
        let mut fields: Vec<Field> = obj.fields.iter().map(|(k, f)| Field {
            name: k.clone(),
            ty: lower_to_ir(&f.ty),
            required: f.non_null_in == obj.seen_objects,
        }).collect();
        fields.sort_by(|a, b| a.name.cmp(&b.name));
        arms.push(Ty::Object { fields });
    }

    // 3) Numbers
    if let Some(num) = &u.num {
        let integerish = (num.saw_int || num.saw_uint) && !num.saw_float
            && num.min_f64.0.is_finite() && num.max_f64.0.is_finite()
            && num.min_f64.0.fract() == 0.0 && num.max_f64.0.fract() == 0.0;

        if integerish {
            arms.push(Ty::Integer {
                min: Some(num.min_f64.0 as i64),
                max: Some(num.max_f64.0 as i64),
            });
        } else {
            arms.push(Ty::Number {
                min: if num.min_f64.0.is_finite() { Some(num.min_f64.0) } else { None },
                max: if num.max_f64.0.is_finite() { Some(num.max_f64.0) } else { None },
            });
        }
    }

    // 4) Strings
    if let Some(str_c) = &u.str_ {
        // // Priority: synthesized pattern (grex) > LCP-based restricted tail > none
        // let pattern = if let Some(rx) = &str_c.pattern_synth {
        //     Some(rx.clone())
        // } else if let Some(p) = &str_c.lcp {
        //     if p.len() >= LCP_MIN_FOR_PATTERN {
        //         // Anchored + restricted tail (never naked '.*')
        //         Some(format!(r"^{}[A-Za-z0-9._%+\-]*$", crate::inference::str::escape_regex(p)))
        //     } else {
        //         None
        //     }
        // } else {
        //     None
        // };

        

        // Priority is simple now:
        //   - If grex produced a pattern, use it.
        //   - Else if we have a tiny enum, keep it.
        //   - Else arbitrary string.
        let pattern = str_c.pattern_synth.clone();


        let mut enum_: Vec<String> = str_c.lits.iter().cloned().collect();
        enum_.sort();

        arms.push(Ty::String { enum_, pattern, format_uri: str_c.is_uri });
    }


    // 5) Bool
    if u.has_bool {
        arms.push(Ty::Bool);
    }

    match arms.len() {
        0 => Ty::Null,
        1 => arms.remove(0),
        _ => simplify_unions(arms),
    }
}

// Collapse common unions: X ∪ null → Nullable(X)
fn simplify_unions(mut arms: Vec<Ty>) -> Ty {
    let mut had_null = false;
    arms.retain(|t| {
        if matches!(t, Ty::Null) { had_null = true; false } else { true }
    });

    let core = match arms.len() {
        0 => Ty::Null,
        1 => arms.remove(0),
        _ => Ty::OneOf(arms),
    };

    if had_null {
        Ty::Nullable(Box::new(core))
    } else {
        core
    }
}
