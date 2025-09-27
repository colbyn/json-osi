use std::collections::BTreeMap;
use super::U;

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

impl ObjC {
    pub(super) fn join(a: &Self, b: &Self) -> Self {
        let mut out = Self::default();
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
                        ty: U::join(&fa.ty, &fb.ty),
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
}

