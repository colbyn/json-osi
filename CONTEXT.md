# Evidence-Driven Schema Inference → Typed IR → Strict Rust

This is the high-level map for future-you (or a careful AI). It explains what the tool does, why it’s strict, and where to adjust policies without derailing rigor.

---

## 1) What this is

**Goal:** Learn a compact structural summary from messy JSON samples (including obfuscated, position-oriented arrays with null padding), then either:
- emit a small **JSON-Schema-ish** debug view, or
- generate **strict Rust types** + deserializers that fail fast on shape drift.

**Design stance:** Collect evidence first, decide later. Lists vs tuples, integers vs floats, enum vs pattern are all decisions made from aggregated signals, not ad-hoc if-chains.

---

## 2) End-to-end flow

1) **Observe** (`inference::observe_value`): Convert a JSON value into a bounded summary `U` that tracks, per kind, the minimal signals we need (counts, min/max, LCP, etc.). Arrays collect *both* pooled list evidence and per-position tuple evidence.

2) **Join (⊔)** (`inference::join`): Merge summaries. Commutative, associative, idempotent → order-independent learning with no retained samples (only sufficient statistics).

3) **Normalize** (`inference::normalize`): Apply small, centralized policies:
   - Drop subsumed numeric literals, keep intervals; integer vs number by evidence.
   - Strings: keep tiny human-ish enums or collapse to **pattern** by **LCP**; flag simple URIs.
   - Arrays: decide tuple vs list using distributional signals (below).
   - Objects: compute “required-ish” via non-null counts.

4) **Lower to IR** (`lower::lower_to_ir`): Produce a serde-free, typed IR (`ir::Ty`). Nullability becomes `Nullable(T)` unless the node is *exactly null* (then `Null`).

5) **Codegen** (`codegen::Codegen::emit`): Emit strict Rust:
   - exact tuple arity via tuple structs
   - `#[serde(deny_unknown_fields)]` on objects
   - transparent newtypes for numbers/strings with bounds/regex/URI checks
   - tiny string sets → strict Rust enums
   - tagless unions → try-each-arm deserializer

---

## 3) Summary structure `U` (CNF/LUB node)

Each node has **≤ 1 arm per kind**:

- `nullable: bool`, `has_bool: bool`
- `num: Option<NumC>`: `{ min_f64, max_f64, lits_f64 (capped), saw_int/uint/float }`
- `str_: Option<StrC>`: `{ lits (capped), lcp, is_uri }`
- `arr: Option<ArrC>`:
  - `item: Box<U>` (pooled list hypothesis)
  - `cols: Vec<U>` (per-position tuple hypothesis)
  - `present[i]` / `non_null[i]`, `len_min/len_max`, `samples`
- `obj: Option<ObjC>`: `fields: { name -> FieldC { ty: U, present_in, non_null_in } }`, `seen_objects`

**Array observation rule:** Always update both `item` and `cols` + counts; pad missing positions with a **nullable placeholder** so sparsity/optional-tail is preserved across joins.

---

## 4) Normalize policies (single source of truth)

Numbers
- Keep min/max interval.
- Prefer `integer` iff **only** ints/uints observed and bounds are integral.
- Numeric literal sets are capped and often pruned (interval subsumes atoms).

Strings
- Recompute LCP from all literals.
- Keep **tiny, human-ish** enums (size/length thresholds); otherwise drop literals.
- If LCP is strong (thresholded) or flagged URI, emit **pattern** / `format:"uri"`.

Arrays – **Tuple vs List decision** (`decide_tuple`)
- Need ≥ 2 arrays; signals for tuple include:
  - **Exact-null pads** (present==samples && non_null==0).
  - **Requiredness contrast** (≥90% present for some positions).
  - **Kind divergence** between a column and the pooled `item`.
  - **Numeric overlap** between per-position vs pooled intervals is weak.
  - **String LCP divergence** between column vs pooled.
- If signals weak → collapse to list (keep `item`; clear positional evidence).

Objects
- Normalize fields recursively. `required` in IR uses `non_null_in == seen_objects`.

---

## 5) Typed IR (`ir.rs`)

```
Ty::{
    Null | Bool |
    Integer{min,max} | Number{min,max} |
    String{enum\_,pattern,format\_uri} |
    ArrayList{item,min\_items,max\_items} |
    ArrayTuple{elems,min\_items,max\_items} |
    Object{fields: Vec\<Field{name,ty,required}} |
    OneOf(Vec<Ty>) | Nullable(Box<Ty>)
}
```

Lowering rules:
- “Exactly null” node → `Ty::Null` (not `Nullable<Null>`).
- Otherwise lower and wrap in `Nullable` if `u.nullable`.
- Arrays use normalized tuple/list choice; `min_items` for tuples is `last_required_idx + 1`, where **exact-null pads are required** by value.

Unions:
- `OneOf(T, Null)` → `Nullable(T)` simplification.
- Otherwise keep `OneOf` and let codegen build a tagless enum that tries each arm strictly.

---

## 6) Rust codegen (`codegen.rs`)

Header imports: `serde::{Deserialize, Deserializer}`, `serde::de::Error as DeError`, `once_cell::sync::Lazy`, `regex::Regex`.

Primitives:
- `Null` type with a custom `Deserialize` that only accepts JSON `null`.
- Numbers → transparent newtypes over `i64` / `f64` with min/max checks and finite checks.
- Strings:
  - tiny enums → Rust `enum` with strict `Deserialize` and `Serialize`.
  - pattern/URI → transparent newtype + precompiled `Regex` or scheme check; implement `Deref<Target=String>`.

Objects:
- `#[serde(deny_unknown_fields)]`; required vs `Option<_>` per IR.

Arrays:
- Tuples → `pub struct Type(A, B, ...)` exact arity; columns that are optional become `Option<_>`; pads that **must** be null use `Null`.
- Lists → `Vec<T>` (enforcing `min_items`/`max_items` at deserialize time is an optional future extension).

Unions:
- Tagless enum; deserialize by trying arms in order; fail with a helpful message.

Naming:
- Type names: CamelCase, keywords avoided, prefix added if needed.
- Field names: snake_case, keywords avoided.
- Enum variants: sanitized PascalCase with collision-avoidance via short hash.

---

## 7) CLI surface (`cli.rs`)

Subcommands:
- `schema` → emit JSON-Schema-ish debug view.
- `rust` → emit strict Rust types.

Shared flags (`InputSettings`):
- `--ndjson` (present but not used yet)
- `--json-pointer /path/to/node`
- `--jq-expr 'jq filter'` (via `jaq`, returns 0+ JSON texts)
- `-i, --input <paths|globs>...` (required; stdin not yet supported)

Per command:
- `schema -o <file.json>` (stdout if omitted)
- `rust --root-type <Name> -o <file.rs>` (stdout if omitted)

Input resolution:
- Glob patterns expanded (`* ? [ {` detection).
- Literal paths accepted.
- Each file: read → parse → optional `jq` → apply results to inference.

---

## 8) Tests (quick map)

- Join laws (idempotent/commutative/associative) at emitter level.
- Integer detection.
- String LCP → pattern; tiny enums kept.
- Array tuple optional tail; requiredness logic.
- Nested lat/lon: per-position numeric bounds propagate.

---

## 9) Policy knobs (safe places to tweak)

- `LCP_MIN_FOR_PATTERN`, `STRING_ENUM_MAX`, `STRING_ENUM_MAX_LEN`
- literal caps: `MAX_STR_LITS`, `MAX_NUM_LITS`
- tuple decision thresholds (e.g., overlap `< 0.3`)
- whether numeric atoms outside interval are kept/dropped

All reside near `inference.rs` top; changing them won’t ripple through core logic.

---

## 10) Philosophy (guardrails)

- **Evidence before commitment.** Always carry both list+tuple hypotheses until normalize.
- **Strict by default.** It’s better to reject bad rows than silently coerce.
- **Deterministic & explainable.** Keep codegen deterministic. Centralize heuristics.
- **Typed boundary = contract.** The Rust model is the contract; the schema emitter is a window into the learned shape.

Integration sketch:

```
let mut inf = Inference::new();
for v in inputs { inf.observe\_value(\&v); }
let u  = inf.solve();
let ir = lower::lower\_to\_ir(\&u);
let mut cg = codegen::Codegen::new();
cg.emit(\&ir, "Root");
let rust\_src = cg.into\_string();

```

If you extend the system: add *measurable signals* to `U` + normalize, thread them through IR, and keep the codegen small and strict.
