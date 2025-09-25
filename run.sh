set -e

# cargo run --release -- rust --input 'examples/samples.json' --jq-expr='.[]' --out .output/models.rs --track-time

cargo run --release -- gen \
    -i 'examples/samples.json' \
    --jq-expr '.[]' \
    --rust   .output/cg/latest.rs \
    --schema .output/cg/latest.json \
    --ir-debug .output/cg/latest.debug \
    --track-time
