pub mod cli;
pub mod codegen;
pub mod inference;
pub mod ir;
pub mod jq_exec;
pub mod norm_ir;
pub mod path_de;

use serde_json::{json, Value};

/// Realistic proto-like payload samples:
/// - Heterogeneous, position-based tuples (arrays) with padded nulls
/// - Homogeneous lists (arrays of scalars), sometimes with nulls
///
/// Tuple layout we’re pretending to reverse-engineer.
#[allow(unused)]
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


fn main() {
    // run_basic_test_samples();
    // run_real_world_samples();
    let command_line_interface = cli::CommandLineInterface::load();
    // eprintln!("{command_line_interface:#?}");
    command_line_interface.run();
}
