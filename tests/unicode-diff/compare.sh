#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
go_bin="${GO:-go}"

go_version="$("$go_bin" version | awk '{print $3}')"
case "$go_version" in
    go1.27.*) ;;
    *)
        printf 'expected Go 1.27.x, got %s\n' "$go_version" >&2
        exit 2
        ;;
esac

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

"$go_bin" run "$script_dir/go/main.go" > "$tmp_dir/go-unicode-mask.tsv"
CARGO_TARGET_DIR="$tmp_dir/cargo-target" cargo run \
    --quiet \
    --locked \
    --manifest-path "$script_dir/rust/Cargo.toml" \
    -- "$tmp_dir/go-unicode-mask.tsv" "$@"
