#!/bin/sh
# Real RAR files for the probe, since RAR cannot be written without WinRAR:
# libarchive's test archives (BSD-2; RAR4 and RAR5, encrypted, solid, split,
# and many deliberately broken ones from fuzzing) and unrar.rs's test data.
# They land in corpus/rar/, which is not checked in.
#
#   sh scripts/fetch-rar-samples.sh
#   SPIKE_RAR_DIR=corpus/rar cargo run --release -p spike-probe
set -eu

out="$(pwd)/corpus/rar"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$out/libarchive" "$out/unrar-rs"

git clone -q --depth 1 --filter=blob:none --sparse https://github.com/libarchive/libarchive "$work/libarchive"
git -C "$work/libarchive" sparse-checkout set libarchive/test
for uu in "$work"/libarchive/libarchive/test/*rar*.uu; do
	name="$(basename "$uu" .uu)"
	case "$name" in
	*.rar) uudecode -o "$out/libarchive/$name" "$uu" ;;
	*.rar.Z) uudecode -o "$work/$name" "$uu" && uncompress -c "$work/$name" > "$out/libarchive/${name%.Z}" ;;
	esac
done

git clone -q --depth 1 https://github.com/muja/unrar.rs "$work/unrar.rs"
cp "$work"/unrar.rs/data/*.rar "$out/unrar-rs/"

echo "$(ls "$out/libarchive" | wc -l) libarchive and $(ls "$out/unrar-rs" | wc -l) unrar.rs samples in $out"
