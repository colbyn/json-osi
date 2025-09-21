//! Minimal CLI: infer → (schema | rust)
use std::path::PathBuf;
use clap::{Parser, Subcommand, Args};

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

#[derive(clap::Parser, Debug)]
struct JsonSchemaOut {
    #[command(flatten)]
    input_settings: InputSettings,

    /// output .json file (stdout if omitted)
    #[arg(short, long)]
    out: Option<PathBuf>,

    /// debugging
    #[arg(long)]
    no_op: bool,
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

    /// debugging
    #[arg(long)]
    no_op: bool,
}

// ————————————————————————————————————————————————————————————————————————————
// IMPLEMENTATION
// ————————————————————————————————————————————————————————————————————————————

impl InputSettings {
    fn load_process(&self, mut apply: impl FnMut(serde_json::Value)) {
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
                    apply(json_value)
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
                        apply(json_value)
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
        match &self.cmd {
            Command::Schema(target) => {
                // debug path
                if target.no_op {
                    eprintln!("{self:#?}");
                    return
                }

                // 1) build state
                let mut inf = crate::inference::Inference::new();
                target.input_settings.load_process(|value| {
                    inf.observe_value(&value);
                });
                
                // 3) solve & generate schema
                let u = inf.solve();
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
                // debug path
                if target.no_op {
                    eprintln!("{self:#?}");
                    return
                }
                
                // 1) build state
                let mut inf = crate::inference::Inference::new();
                target.input_settings.load_process(|value| {
                    inf.observe_value(&value);
                });
                let u = inf.solve();
                
                // 2) lower to typed IR
                let ir_root = crate::lower::lower_to_ir(&u);
                
                // 3) generate strict Rust types
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
        }
    }
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