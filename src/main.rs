pub mod inference;
pub mod ir;
pub mod lower;
pub mod codegen;
pub mod cli;
pub mod jq_exec;

use serde_json::{json, Value};

/// Realistic proto-like payload samples:
/// - Heterogeneous, position-based tuples (arrays) with padded nulls
/// - Homogeneous lists (arrays of scalars), sometimes with nulls
///
/// Tuple layout we’re pretending to reverse-engineer.
fn realistic_samples() -> Vec<Value> {
    vec![
        // --- Tuple records (heterogeneous arrays with null padding) ---
        json!([
            "0ahUKEa1ZQ", "Acme Widgets",
            [null, [37.4219, -122.0840], null],
            "https://example.com/a",
            4.3,
            true,
            ["hardware","store"],
            null,
            null
        ]),
        json!([
            "0ahUKEa2ZQ",
            "Acme Widgets - East",
            [null, [37.4200, -122.0830], null],
            null,
            4.5,
            null,
            ["hardware"],
            null,
            null
        ]),
        json!([
            "0ahUKEa3ZQ",
            null,
            [null, [37.4225, -122.0855], null],
            "https://example.com/c",
            null,
            false,
            [],
            null,
            null
        ]),
        json!([
            "0ahUKEa4ZQ",
            "ACME",
            null,
            null,
            4,
            null,
            ["store","tools"],
            null,
            null
        ]),
        json!([
            "0ahUKEa5ZQ",
            "Acme West",
            [null, [37.0000, -122.0000], null],
            "https://example.com/w",
            4.1,
            true,
            ["hardware","outlet"],
            null,
            null
        ]),
        // Extra tuple with longer padding to probe nullable tail behavior
        json!([
            "0ahUKEa6ZQ",
            "Acme Central",
            [null, null, null],
            null,
            null,
            null,
            ["tools"],
            null,
            null
        ]),
    ]
}

#[allow(unused)]
fn run_basic_test_samples() {
    // 1) build state from your realistic samples
    let samples = realistic_samples(); // your function
    {
        let mut inf = inference::Inference::new();
        for v in &samples { inf.observe_value(v); }
        let u = inf.solve();
        
        // 2) lower to typed IR
        let ir_root = lower::lower_to_ir(&u);
        
        // 3) generate strict Rust types
        let mut cg = codegen::Codegen::new();
        cg.emit(&ir_root, "Root");
        let rust_src = cg.into_string();
        
        // 4) print (or write to file)
        println!("{}", rust_src);
    }
    // {
    //     eprintln!("—— testing deserialization ——");
    //     for entry in &samples {
    //         let json_source = serde_json::to_string_pretty(entry).unwrap();
    //         let payload = serde_json::from_str::<crate::models::Root>(&json_source);
    //         let payload = match payload {
    //             Ok(x) => x,
    //             Err(error) => {
    //                 eprintln!("❌ failed: {error}");
    //                 continue;
    //             }
    //         };
    //         eprintln!("✅ success");
    //     }
    // }
}


fn main() {
    // run_basic_test_samples();
    // run_real_world_samples();
    let command_line_interface = cli::CommandLineInterface::load();
    // eprintln!("{command_line_interface:#?}");
    command_line_interface.run();
}
