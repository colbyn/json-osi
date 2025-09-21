set -e

cargo run -- rust --input 'examples/samples.json' --jq-expr='.[]' --out .output/models.rs --track-time
