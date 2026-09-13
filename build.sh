#!/bin/sh
# Compila Nanofy en modo release (Linux / macOS / Git Bash en Windows).
set -e
cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release "$@"
