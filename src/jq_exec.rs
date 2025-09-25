use anyhow::{anyhow, Result};
use jaq_core::{compile::Undefined, load, Compiler, Ctx, RcIter};
use jaq_json::Val;
use serde_json::Value;

pub fn run_jaq(filter_src: &str, input: &Value) -> Result<Vec<String>> {
    let loader = load::Loader::new(jaq_std::defs().chain(jaq_json::defs()));
    let arena = load::Arena::default();
    let program = load::File { code: filter_src, path: () };

    let modules = loader
        .load(&arena, program)
        .map_err(format_parse_errors)?;      // now infers fine

    let filter = Compiler::default()
        .with_funs(jaq_std::funs().chain(jaq_json::funs()))
        .compile(modules)
        .map_err(format_undefined_errors)?;  // ditto

    let inputs = RcIter::new(core::iter::empty());
    let mut it = filter.run((Ctx::new([], &inputs), Val::from(input.clone())));

    let mut out = Vec::new();
    while let Some(item) = it.next() {
        let v = item.map_err(|e| anyhow!(format!("{e:?}")))?; // stringify jaq error
        out.push(format!("{v}")); // Val: Display -> JSON text
    }
    Ok(out)
}

fn format_parse_errors(
    errs: Vec<(load::File<&str, ()>, load::Error<&str>)>,
) -> anyhow::Error {
    let mut s = String::new();
    for (file, err) in errs {
        s.push_str(&format!("parse error: {err:?} in `{}`\n", file.code));
    }
    anyhow::anyhow!(s)
}

fn format_undefined_errors(
    errs: Vec<(load::File<&str, ()>, Vec<(&str, Undefined)>)>,
) -> anyhow::Error {
    let mut s = String::new();
    for (file, list) in errs {
        for (name, undef) in list {
            s.push_str(&format!("undefined `{name}`: {undef:?} in `{}`\n", file.code));
        }
    }
    anyhow::anyhow!(s)
}
