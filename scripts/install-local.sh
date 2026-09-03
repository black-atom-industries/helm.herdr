#!/bin/sh
set -eu

root=$(CDPATH="" cd -- "$(dirname -- "$0")/.." && pwd)
bin_dir=${HOME:?HOME must be set}/.local/bin

cargo build --release --manifest-path "$root/Cargo.toml"
mkdir -p "$bin_dir"
ln -sfn "$root/target/release/helm-herdr" "$bin_dir/helm-herdr"
herdr plugin link "$root"

printf 'Installed helm-herdr at %s\n' "$bin_dir/helm-herdr"
printf 'Linked Herdr plugin from %s\n' "$root"
