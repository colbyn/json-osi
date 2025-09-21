//! Minimal CLI: infer → (schema | rust)
use std::path::{Path, PathBuf};
use clap::{Parser, Subcommand, Args};
use rayon::prelude::*;
use serde_json::Value;

use crate::inference::{U, observe_value, join, normalize};

// ————————————————————————————————————————————————————————————————————————————
// TYPES
// ————————————————————————————————————————————————————————————————————————————

/// infer structure from JSON/NDJSON and output either a JSON schema-ish view or a strict Rust model
#[derive(Parser, Debug)]
pub struct CommandLineInterface {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// infer and print the JSON-schema-ish debug view
    Schema(JsonSchemaOut),
    /// infer and emit a strict Rust data model
    Rust(RustOut),
    /// Pre-processing CLI helpers
    PreProcess(PreProcess)
}

#[derive(Args, Debug, Clone)]
struct InputSettings {
    /// treat input as newline-delimited JSON (NDJSON)
    #[arg(long, default_value_t = false)]
    ndjson: bool,

    /// JSON Pointer to select a subnode in each document (e.g. /data/items/0/payload)
    #[arg(long)]
    json_pointer: Option<String>,
    
    /// JQ pre-process filter for each document.
    #[arg(long)]
    jq_expr: Option<String>,

    /// One or more inputs. May be literal paths or quoted glob patterns or '-' for stdin
    /// 
    /// TODO: stdin not yet supported
    #[arg(long, short, num_args = 1.., required = true)]
    input: Vec<String>,
}

#[derive(Args, Debug, Clone)]
struct CommonSettings {
    /// Debugging: print CLI invocation settings and then terminate
    #[arg(long)]
    no_op: bool,
    
    /// Debugging: track elapsed time and then print to stdout
    #[arg(long)]
    track_time: bool,

    /// Debugging: disable parallelization
    #[arg(long)]
    no_parallel: bool,
}

#[derive(clap::Parser, Debug)]
struct JsonSchemaOut {
    #[command(flatten)]
    input_settings: InputSettings,

    /// output .json file (stdout if omitted)
    #[arg(short, long)]
    out: Option<PathBuf>,

    #[command(flatten)]
    common_settings: CommonSettings,
}


#[derive(clap::Parser, Debug)]
struct RustOut {
    #[command(flatten)]
    input_settings: InputSettings,

    /// top-level Rust type name
    #[arg(long, default_value = "Root")]
    root_type: String,

    /// output .rs file (stdout if omitted)
    #[arg(short, long)]
    out: Option<PathBuf>,

    #[command(flatten)]
    common_settings: CommonSettings,
}

#[derive(clap::Parser, Debug)]
struct PreProcess {
    // #[command(flatten)]
    // input_settings: InputSettings,

    // /// top-level Rust type name
    // #[arg(long, default_value = "Root")]
    // root_type: String,

    // /// 
    // #[arg(short, long)]
    // out: Option<PathBuf>,

    // #[command(flatten)]
    // common_settings: CommonSettings,
}

// ————————————————————————————————————————————————————————————————————————————
// IMPLEMENTATION
// ————————————————————————————————————————————————————————————————————————————

impl InputSettings {
    fn load_process(&self, mut apply: impl FnMut(&Path, serde_json::Value)) {
        let source_paths = resolve_file_path_patterns(&self.input).expect(
            "failed to resolve input file paths"
        );
        for source_path in source_paths {
            let source_path_str = source_path.to_string_lossy().to_string();
            let source = std::fs::read_to_string(&source_path);
            let source = match source {
                Ok(x) => x,
                Err(error) => {
                    panic!("Failed to read source file: {error}");
                }
            };
            let json_value = serde_json::from_str::<serde_json::Value>(&source);
            let json_value = match json_value {
                Ok(x) => x,
                Err(error) => {
                    panic!("Failed to parse JSON source file ({source_path_str}): {error}");
                }
            };
            match self.jq_expr.as_ref() {
                None => {
                    apply(&source_path, json_value)
                },
                Some(jq_expr) => {
                    let result = crate::jq_exec::run_jaq(jq_expr, &json_value);
                    let result = match result {
                        Ok(xs) => xs,
                        Err(error) => {
                            panic!("Failed to apply jq expression to source file ({source_path_str}): {error}");
                        }
                    };
                    for json_value in result.into_iter() {
                        let json_value = serde_json::from_str::<serde_json::Value>(&json_value);
                        let json_value = match json_value {
                            Ok(x) => x,
                            Err(error) => {
                                panic!("Failed to parse JSON source file ({source_path_str}): {error}");
                            }
                        };
                        apply(&source_path, json_value)
                    }
                }
            }
        }
    }
}

impl CommandLineInterface {
    pub fn load() -> Self {
        Self::parse()
    }
    pub fn run(&self) {
        let start = std::time::Instant::now();
        let mut print_elapsed_time = false;

        match &self.cmd {
            Command::Schema(target) => {
                // - DEBUG PATH -
                if target.common_settings.no_op {
                    let mut sources = Vec::<PathBuf>::new();
                    target.input_settings.load_process(|source_path, _| {
                        sources.push(source_path.to_path_buf());
                    });
                    eprintln!("{self:#?}");
                    eprintln!("RESOLVED SOURCES:");
                    for source in sources {
                        eprintln!("\t- {}", source.to_string_lossy());
                    }
                    return
                }
                if target.common_settings.track_time {
                    print_elapsed_time = true;
                }

                // - BUILD STATE -
                let u = compute_merge_summary(&target.input_settings, &target.common_settings);

                // COMPUTE FINALIZED SCHEMA
                let schema = crate::inference::emit_schema(&u);
                let schema_src = serde_json::to_string_pretty(&schema).unwrap();
                if let Some(out) = target.out.as_ref() {
                    if let Some(parent) = out.parent() {
                        std::fs::create_dir_all(parent).unwrap();
                    }
                    std::fs::write(&out, &schema_src).unwrap();
                } else {
                    println!("{schema_src}");
                }
            }
            Command::Rust(target) => {
                // DEBUG PATH
                if target.common_settings.no_op {
                    let mut sources = Vec::<PathBuf>::new();
                    target.input_settings.load_process(|source_path, _| {
                        sources.push(source_path.to_path_buf());
                    });
                    eprintln!("{self:#?}");
                    eprintln!("RESOLVED SOURCES:");
                    for source in sources {
                        eprintln!("\t- {}", source.to_string_lossy());
                    }
                    return
                }
                if target.common_settings.track_time {
                    print_elapsed_time = true;
                }
                
                // - BUILD STATE -
                let u = compute_merge_summary(&target.input_settings, &target.common_settings);
                
                // - LOWER TO TYPED IR -
                let ir_root = crate::lower::lower_to_ir(&u);
                
                // - GENERATE STRICT RUST TYPES -
                let mut cg = crate::codegen::Codegen::new();
                cg.emit(&ir_root, "Root");
                let rust_src = cg.into_string();

                if let Some(out) = target.out.as_ref() {
                    if let Some(parent) = out.parent() {
                        std::fs::create_dir_all(parent).unwrap();
                    }
                    std::fs::write(&out, &rust_src).unwrap();
                } else {
                    println!("{rust_src}");
                }
            }
            Command::PreProcess(target) => {
                let _ = target; // TODO
            }
        }

        if print_elapsed_time {
            let elapsed = start.elapsed();
            eprintln!("inference took {}", format_duration(elapsed));
        }
    }
}

fn compute_merge_summary(input_settings: &InputSettings, common_settings: &CommonSettings) -> U {
    if common_settings.no_parallel {
        let mut inf = crate::inference::Inference::new();
        input_settings.load_process(|_, value| {
            inf.observe_value(&value);
        });
        let u = inf.solve();
        return u
    }

    // 1) resolve paths (same behavior as your sequential path)
    let source_paths = resolve_file_path_patterns(&input_settings.input)
        .expect("failed to resolve input file paths");

    let ndjson = input_settings.ndjson;
    let jq_expr = input_settings.jq_expr.clone();
    let json_ptr = input_settings.json_pointer.clone();

    // 2) MAP (parallel): per-file local summary
    let combined = source_paths.par_iter()
        .map(|path| {
            let path_str = path.to_string_lossy().to_string();
            let src = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("read failed ({path_str}): {e}"));

            let mut acc = U::empty();

            // helper: apply jq (if any), yielding 0+ JSON values
            let mut apply_one = |v: &Value| {
                let vals: Vec<Value> = match jq_expr.as_ref() {
                    None => vec![v.clone()],
                    Some(expr) => {
                        let out = crate::jq_exec::run_jaq(expr, v)
                            .unwrap_or_else(|e| panic!("jq failed ({path_str}): {e}"));
                        out.into_iter()
                           .map(|t| serde_json::from_str::<Value>(&t)
                                .unwrap_or_else(|e| panic!("jq output not JSON ({path_str}): {e}\n{t}")))
                           .collect()
                    }
                };

                for pv in vals {
                    // JSON Pointer: if it selects an array, expand; else take the node
                    if let Some(ptr) = json_ptr.as_ref() {
                        match pv.pointer(ptr.as_str()) {
                            None => { /* zero samples at this file for this node */ }
                            Some(Value::Array(xs)) => {
                                for sub in xs {
                                    let obs = observe_value(sub);
                                    acc = join(&acc, &obs);
                                }
                            }
                            Some(other) => {
                                let obs = observe_value(other);
                                acc = join(&acc, &obs);
                            }
                        }
                    } else {
                        let obs = observe_value(&pv);
                        acc = join(&acc, &obs);
                    }
                }
            };

            if ndjson {
                for (i, line) in src.lines().enumerate() {
                    let line = line.trim();
                    if line.is_empty() { continue; }
                    let v: Value = serde_json::from_str(line)
                        .unwrap_or_else(|e| panic!("NDJSON parse error {path_str}:{}: {e}\n{line}", i+1));
                    apply_one(&v);
                }
            } else {
                let root: Value = serde_json::from_str(&src)
                    .unwrap_or_else(|e| panic!("JSON parse error ({path_str}): {e}"));
                apply_one(&root);
            }

            acc
        })
        // 3) REDUCE (parallel): merge thread-local summaries
        .reduce(|| U::empty(), |a, b| join(&a, &b));

    // 4) normalize once at the end (same as Inference::solve)
    let mut u = combined;
    normalize(&mut u);
    u
}


// ————————————————————————————————————————————————————————————————————————————
// INTERNAL HELPERS
// ————————————————————————————————————————————————————————————————————————————

fn resolve_file_path_patterns<I>(patterns: I) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>>
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    fn has_glob_chars(s: &str) -> bool {
        // Minimal glob detection for the `glob` crate syntax.
        s.bytes().any(|b| matches!(b, b'*' | b'?' | b'[' | b'{' ))
    }

    let mut out = Vec::<PathBuf>::new();

    for raw in patterns {
        let pattern = raw.as_ref();

        if has_glob_chars(pattern) {
            // Treat as a glob pattern
            let mut matched_any = false;
            for entry in glob::glob(pattern)? {
                match entry {
                    Ok(p) => {
                        matched_any = true;
                        out.push(p);
                    }
                    Err(e) => return Err(Box::new(e)),
                }
            }
            if !matched_any {
                // Pattern was explicitly a glob but matched nothing -> surface as an error
                return Err(format!("glob pattern matched no files: {pattern}").into());
            }
        } else {
            // Treat as a literal path
            out.push(PathBuf::from(pattern));
        }
    }

    Ok(out)
}

fn format_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();

    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m {}s", secs / 3600, (secs % 3600) / 60, secs % 60)
    }
}

