use super::U;

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

impl ArrC {
    pub(super) fn join(a: &Self, b: &Self) -> Self {
        let mut out = Self::default();
        out.len_min = a.len_min.min(b.len_min);
        out.len_max = a.len_max.max(b.len_max);
        out.samples = a.samples + b.samples;
        out.item = Box::new(U::join(&a.item, &b.item));
    
        let n = a.cols.len().max(b.cols.len());
        out.cols = (0..n).map(|i| {
            let ai = a.cols.get(i).cloned().unwrap_or_else(missing_nullable);
            let bi = b.cols.get(i).cloned().unwrap_or_else(missing_nullable);
            U::join(&ai, &bi)
        }).collect();
    
        out.present = (0..n).map(|i| {
            a.present.get(i).copied().unwrap_or(0) + b.present.get(i).copied().unwrap_or(0)
        }).collect();
    
        out.non_null = (0..n).map(|i| {
            a.non_null.get(i).copied().unwrap_or(0) + b.non_null.get(i).copied().unwrap_or(0)
        }).collect();
    
        out
    }
}

fn missing_nullable() -> U { let mut u = U::empty(); u.nullable = true; u }
