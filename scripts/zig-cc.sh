#!/usr/bin/env bash
set -euo pipefail

# Wrapper script for zig cc to map x86_64-unknown-linux-gnu -> x86_64-linux-gnu
new_args=""
for arg in "$@"; do
    case "$arg" in
        --target=x86_64-unknown-linux-gnu)
            new_args="$new_args --target=x86_64-linux-gnu"
            ;;
        --target=x86_64-unknown-linux-musl)
            new_args="$new_args --target=x86_64-linux-musl"
            ;;
        *)
            new_args="$new_args $arg"
            ;;
    esac
done

exec zig cc $new_args
