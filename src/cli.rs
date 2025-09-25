//! Unified CLI: infer → lower → emit {schema,rust,ir-debug}
//! Usage examples:
//!   json-osi gen -i data.json --jq-expr '.[]' --schema -            # print schema to stdout
//!   json-osi gen -i data.json --rust out/models.rs                  # write Rust
//!   json-osi gen -i data.json --schema out/schema.json --rust -     # both; Rust to stdout
//!   json-osi gen -i '-' --ndjson --rust out.rs                      # read NDJSON from stdin

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use colored::Colorize;

use clap::{Args, Parser, Subcommand, ValueEnum};
use rayon::prelude::*;
use serde_json::Value;

use crate::inference::{join, normalize, observe_value, U};

/// Top-level CLI
#[derive(Parser, Debug)]
#[command(name = "json-osi", version, about = "Evidence-driven schema inference + strict Rust codegen")]
pub struct CommandLineInterface {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Generate one or more outputs in a single pass
    Gen(Gen),
    /// Legacy commands (kept for a gentle migration)
    #[command(hide = true)]
    Schema(LegacyJsonSchemaOut),
    #[command(hide = true)]
    Rust(LegacyRustOut),
}

#[derive(Args, Debug, Clone)]
struct InputSettings {
    /// Treat input as newline-delimited JSON (NDJSON)
    #[arg(long, default_value_t = false)]
    ndjson: bool,

    /// JSON Pointer to select a subnode in each document (e.g. /data/items/0/payload)
    #[arg(long)]
    json_pointer: Option<String>,

    /// JQ pre-process filter for each document (via `jaq`)
    #[arg(long)]
    jq_expr: Option<String>,

    /// One or more inputs:
    /// - literal paths
    /// - quoted glob patterns
    /// - '-' for stdin
    #[arg(long, short, num_args = 1.., required = true, value_name = "PATH|GLOB|-")]
    input: Vec<String>,
}

#[derive(Args, Debug, Clone)]
struct CommonSettings {
    /// Debug: print CLI invocation and resolved sources, then exit
    #[arg(long)]
    no_op: bool,

    /// Debug: track elapsed time and print human-friendly duration to stderr
    #[arg(long)]
    track_time: bool,

    /// Debug: disable rayon parallelism
    #[arg(long)]
    no_parallel: bool,
}

/// Unified generator: choose any combination of outputs.
/// For any output flag, pass `-` to write to stdout.
#[derive(Args, Debug)]
struct Gen {
    #[command(flatten)]
    input: InputSettings,

    /// Top-level Rust type name (when emitting Rust)
    #[arg(long, default_value = "Root")]
    root_type: String,

    /// Emit JSON Schema to file (or '-' for stdout)
    #[arg(long, value_name = "FILE|-")]
    schema: Option<PathBuf>,

    /// Emit strict Rust models to file (or '-' for stdout)
    #[arg(long, value_name = "FILE|-")]
    rust: Option<PathBuf>,

    /// Emit a pretty-printed debug view of the lowered IR (not JSON; uses Debug)
    #[arg(long = "ir-debug", value_name = "FILE|-")]
    ir_debug: Option<PathBuf>,

    /// Optional: choose one or more streams to also print to stdout (redundant with '-' paths)
    #[arg(long = "stdout", value_enum)]
    stdout_streams: Vec<StdoutStream>,

    #[command(flatten)]
    common: CommonSettings,
}

#[derive(Copy, Clone, Debug, ValueEnum, Eq, PartialEq)]
enum StdoutStream {
    Schema,
    Rust,
    IrDebug,
}

// --------------------------- Legacy (hidden) ---------------------------

#[derive(Args, Debug)]
struct LegacyJsonSchemaOut {
    #[command(flatten)]
    input_settings: InputSettings,
    /// output .json file (stdout if omitted)
    #[arg(short, long)]
    out: Option<PathBuf>,
    #[command(flatten)]
    common_settings: CommonSettings,
}

#[derive(Args, Debug)]
struct LegacyRustOut {
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

// --------------------------- Impl ---------------------------

impl CommandLineInterface {
    pub fn load() -> Self {
        Self::parse()
    }

    pub fn run(&self) {
        match &self.cmd {
            Command::Gen(cfg) => run_gen(cfg),
            Command::Schema(old) => run_legacy_schema(old),
            Command::Rust(old) => run_legacy_rust(old),
        }
    }
}

// --------------------------- gen ---------------------------

fn run_gen(cfg: &Gen) {
    let start = std::time::Instant::now();
    let print_elapsed_time = cfg.common.track_time;

    // Debug no-op prints invocation and resolved files
    if cfg.common.no_op {
        eprintln!("{}", format!("INVOCATION: {}", format!("{cfg:#?}").red()).cyan());
        cfg.input.load_process(|_, _| {
            () // NO-OP
        });
        return;
    }

    // At least one target?
    if cfg.schema.is_none() && cfg.rust.is_none() && cfg.ir_debug.is_none()
        && cfg.stdout_streams.is_empty()
    {
        eprintln!("error: no outputs requested. Use one or more of --schema, --rust, --ir-debug, or --stdout …");
        std::process::exit(2);
    }

    // Build merged & normalized summary
    let u = compute_merge_summary(&cfg.input, &cfg.common);

    // Lower IR once; reuse for multiple emits
    let ir_root = crate::lower::lower_to_ir(&u);

    // 1) Schema
    if cfg.schema.is_some() || cfg.stdout_streams.contains(&StdoutStream::Schema) {
        let schema = crate::inference::emit_schema(&u);
        let schema_src = serde_json::to_string_pretty(&schema).unwrap();

        // file target
        if let Some(path) = cfg.schema.as_ref() {
            write_sink(path, &schema_src).unwrap();
        }

        // stdout stream (if requested, even if also wrote file)
        if cfg.stdout_streams.contains(&StdoutStream::Schema) && cfg.schema.as_deref() != Some(Path::new("-")) {
            println!("{schema_src}");
        }
    }

    // 2) Rust
    if cfg.rust.is_some() || cfg.stdout_streams.contains(&StdoutStream::Rust) {
        let mut cg = crate::codegen::Codegen::new();
        cg.emit(&ir_root, &cfg.root_type);
        let rust_src = cg.into_string();

        if let Some(path) = cfg.rust.as_ref() {
            write_sink(path, &rust_src).unwrap();
        }
        if cfg.stdout_streams.contains(&StdoutStream::Rust) && cfg.rust.as_deref() != Some(Path::new("-")) {
            println!("{rust_src}");
        }
    }

    // 3) IR debug (human pretty; not JSON)
    if cfg.ir_debug.is_some() || cfg.stdout_streams.contains(&StdoutStream::IrDebug) {
        let ir_txt = format!("{:#?}", ir_root);

        if let Some(path) = cfg.ir_debug.as_ref() {
            write_sink(path, &ir_txt).unwrap();
        }
        if cfg.stdout_streams.contains(&StdoutStream::IrDebug) && cfg.ir_debug.as_deref() != Some(Path::new("-")) {
            println!("{ir_txt}");
        }
    }

    if print_elapsed_time {
        let elapsed = start.elapsed();
        eprintln!("inference took {}", format_duration(elapsed));
    }
}

// --------------------------- legacy shims ---------------------------

fn run_legacy_schema(target: &LegacyJsonSchemaOut) {
    if target.common_settings.no_op {
        let mut sources = Vec::<PathBuf>::new();
        target.input_settings.load_process(|p, _| sources.push(p.to_path_buf()));
        eprintln!("{target:#?}");
        eprintln!("RESOLVED SOURCES:");
        for s in sources {
            eprintln!("  - {}", s.to_string_lossy());
        }
        return;
    }

    let u = compute_merge_summary(&target.input_settings, &target.common_settings);
    let schema = crate::inference::emit_schema(&u);
    let schema_src = serde_json::to_string_pretty(&schema).unwrap();

    if let Some(out) = target.out.as_ref() {
        write_sink(out, &schema_src).unwrap();
    } else {
        println!("{schema_src}");
    }
}

fn run_legacy_rust(target: &LegacyRustOut) {
    if target.common_settings.no_op {
        let mut sources = Vec::<PathBuf>::new();
        target.input_settings.load_process(|p, _| sources.push(p.to_path_buf()));
        eprintln!("{target:#?}");
        eprintln!("RESOLVED SOURCES:");
        for s in sources {
            eprintln!("  - {}", s.to_string_lossy());
        }
        return;
    }

    let u = compute_merge_summary(&target.input_settings, &target.common_settings);
    let ir_root = crate::lower::lower_to_ir(&u);
    let mut cg = crate::codegen::Codegen::new();
    cg.emit(&ir_root, "Root");
    let rust_src = cg.into_string();

    if let Some(out) = target.out.as_ref() {
        write_sink(out, &rust_src).unwrap();
    } else {
        println!("{rust_src}");
    }
}

// --------------------------- Core pipeline ---------------------------

fn compute_merge_summary(input_settings: &InputSettings, common_settings: &CommonSettings) -> U {
    if common_settings.no_parallel {
        let mut inf = crate::inference::Inference::new();
        input_settings.load_process(|_, value| {
            inf.observe_value(&value);
        });
        let mut u = inf.solve();
        normalize(&mut u);
        return u;
    }

    let source_paths =
        resolve_file_path_patterns(&input_settings.input).expect("failed to resolve input file paths");

    let ndjson = input_settings.ndjson;
    let jq_expr = input_settings.jq_expr.clone();
    let json_ptr = input_settings.json_pointer.clone();

    let combined = source_paths
        .par_iter()
        .map(|path| {
            let path_str = path.to_string_lossy().to_string();

            // Read source (supports '-' stdin)
            let src = if path_str == "-" {
                let mut buf = String::new();
                io::stdin().read_to_string(&mut buf).expect("failed to read stdin");
                buf
            } else {
                std::fs::read_to_string(path)
                    .unwrap_or_else(|e| panic!("read failed ({path_str}): {e}"))
            };

            let mut acc = U::empty();

            let mut apply_one = |v: &Value| {
                let vals: Vec<Value> = match jq_expr.as_ref() {
                    None => vec![v.clone()],
                    Some(expr) => {
                        let out = crate::jq_exec::run_jaq(expr, v)
                            .unwrap_or_else(|e| panic!("jq failed ({path_str}): {e}"));
                        out.into_iter()
                            .map(|t| {
                                serde_json::from_str::<Value>(&t).unwrap_or_else(|e| {
                                    panic!("jq output not JSON ({path_str}): {e}\n{t}")
                                })
                            })
                            .collect()
                    }
                };

                for pv in vals {
                    if let Some(ptr) = json_ptr.as_ref() {
                        match pv.pointer(ptr.as_str()) {
                            None => {}
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
                    if line.is_empty() {
                        continue;
                    }
                    let v: Value = serde_json::from_str(line).unwrap_or_else(|e| {
                        panic!("NDJSON parse error {path_str}:{}: {e}\n{line}", i + 1)
                    });
                    apply_one(&v);
                }
            } else {
                let root: Value =
                    serde_json::from_str(&src).unwrap_or_else(|e| panic!("JSON parse error ({path_str}): {e}"));
                apply_one(&root);
            }

            acc
        })
        .reduce(|| U::empty(), |a, b| join(&a, &b));

    let mut u = combined;
    normalize(&mut u);
    u
}

// --------------------------- Helpers ---------------------------

fn resolve_file_path_patterns<I>(patterns: I) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>>
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    fn has_glob_chars(s: &str) -> bool {
        s.bytes()
            .any(|b| matches!(b, b'*' | b'?' | b'[' | b'{' ))
    }

    let mut out = Vec::<PathBuf>::new();
    for raw in patterns {
        let p = raw.as_ref();
        if p == "-" {
            out.push(PathBuf::from("-"));
            continue;
        }

        if has_glob_chars(p) {
            let mut matched_any = false;
            for entry in glob::glob(p)? {
                match entry {
                    Ok(path) => {
                        matched_any = true;
                        out.push(path);
                    }
                    Err(e) => return Err(Box::new(e)),
                }
            }
            if !matched_any {
                return Err(format!("glob pattern matched no files: {p}").into());
            }
        } else {
            out.push(PathBuf::from(p));
        }
    }
    let out = out
        .into_iter()
        .map(|x| {
            x.to_str().unwrap().to_owned()
        })
        .collect::<indexmap::IndexSet<_>>()
        .into_iter()
        .map(|x| PathBuf::from(x))
        .collect::<Vec<_>>();
    Ok(out)
}

fn write_sink(path: &Path, contents: &str) -> io::Result<()> {
    if path == Path::new("-") {
        // Write to stdout explicitly (don’t mingle with timing on stderr)
        let mut stdout = io::stdout().lock();
        stdout.write_all(contents.as_bytes())?;
        if !contents.ends_with('\n') {
            stdout.write_all(b"\n")?;
        }
        stdout.flush()?;
        Ok(())
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, contents)
    }
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

trait LoadProcess {
    fn load_process(&self, apply: impl FnMut(&Path, Value));
}

impl LoadProcess for InputSettings {
    fn load_process(&self, mut apply: impl FnMut(&Path, Value)) {
        let source_paths = resolve_file_path_patterns(&self.input).expect("failed to resolve input file paths");

        eprintln!("{}", format!(
            "◦ total source files: {}",
            source_paths.len().to_string().green(),
        ).cyan());

        for source_path in source_paths {
            if let Some(jq_filter) = self.jq_expr.as_ref() {
                eprintln!("{}", format!(
                    "▶︎ processing: {} » '{}'",
                    source_path.to_str().unwrap().green(),
                    jq_filter.blue()
                ).cyan());
            } else {
                eprintln!("{}", format!(
                    "▶︎ processing: {}",
                    source_path.to_str().unwrap().green(),
                ).cyan());
            }
            let path_str = source_path.to_string_lossy().to_string();

            let source = if path_str == "-" {
                let mut buf = String::new();
                io::stdin().read_to_string(&mut buf).expect("failed to read stdin");
                buf
            } else {
                std::fs::read_to_string(&source_path)
                    .unwrap_or_else(|e| panic!("Failed to read source file: {e}"))
            };

            let json_value = serde_json::from_str::<serde_json::Value>(&source).unwrap_or_else(|e| {
                panic!("Failed to parse JSON source file ({path_str}): {e}")
            });

            match self.jq_expr.as_ref() {
                None => apply(&source_path, json_value),
                Some(jq_expr) => {
                    let out = crate::jq_exec::run_jaq(jq_expr, &json_value).unwrap_or_else(|e| {
                        panic!("Failed to apply jq to ({path_str}): {e}")
                    });
                    for txt in out.into_iter() {
                        let v = serde_json::from_str::<serde_json::Value>(&txt).unwrap_or_else(|e| {
                            panic!("jq output not JSON ({path_str}): {e}\n{txt}")
                        });
                        apply(&source_path, v)
                    }
                }
            }
        }
    }
}
