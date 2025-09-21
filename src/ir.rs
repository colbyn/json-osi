// Strongly-typed IR for codegen. No serde_json::Value here.

#[derive(Debug, Clone)]
pub enum Ty {
    Never,                   // unreachable (you can avoid emitting this)
    Null,                    // exactly null
    Bool,
    Integer { min: Option<i64>, max: Option<i64> },
    Number  { min: Option<f64>, max: Option<f64> },
    String  { enum_: Vec<String>, pattern: Option<String>, format_uri: bool },
    ArrayList {
        item: Box<Ty>,
        min_items: Option<u32>,
        max_items: Option<u32>,
    },
    ArrayTuple {
        elems: Vec<Ty>,      // exact arity
        min_items: u32,      // last required index + 1 (exact for tuples)
        max_items: u32,      // == elems.len()
    },
    Object {
        fields: Vec<Field>,  // stable order for deterministic codegen
    },
    OneOf(Vec<Ty>),          // keep small, or rewrite to Nullable where possible
    Nullable(Box<Ty>),       // null wrapper
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub ty: Ty,
    pub required: bool,      // present & non-null in all objects
}
