use std::collections::BTreeSet;
use ordered_float::OrderedFloat;

#[derive(Clone, Debug, Default)]
pub struct NumC {
    pub lits_f64: BTreeSet<OrderedFloat<f64>>,
    pub min_f64: OrderedFloat<f64>,
    pub max_f64: OrderedFloat<f64>,
    pub saw_int: bool,
    pub saw_uint: bool,
    pub saw_float: bool,
}


impl NumC {
    pub(super) fn join(a: &Self, b: &Self) -> Self {
        let mut out = NumC::default();
        out.lits_f64 = &a.lits_f64 | &b.lits_f64;
        if out.lits_f64.len() > super::MAX_NUM_LITS {
            out.lits_f64.clear(); // cap: treat as tokens → interval only
        }
        out.min_f64 = a.min_f64.min(b.min_f64);
        out.max_f64 = a.max_f64.max(b.max_f64);
        out.saw_int = a.saw_int || b.saw_int;
        out.saw_uint = a.saw_uint || b.saw_uint;
        out.saw_float = a.saw_float || b.saw_float;
        out
    }
}
