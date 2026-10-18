#!/bin/sh
set -eu
cd "$(dirname "$0")"
cargo build --locked --release --target aarch64-apple-darwin
cargo build --locked --release --target x86_64-apple-darwin
bundle='dist/MEGA65 DiskLab.app/Contents'
mkdir -p "$bundle/MacOS" "$bundle/Resources"
lipo -create target/aarch64-apple-darwin/release/mega65-disklab target/x86_64-apple-darwin/release/mega65-disklab -output "$bundle/MacOS/mega65-disklab"
cp README.md launcher/Reader-notes.txt launcher/Resident-driver.txt launcher/Resident-demo.bas launcher/LOAD-compatibility.txt "$bundle/Resources/"
cp packaging/Info.plist "$bundle/Info.plist"
codesign --force --deep --sign - 'dist/MEGA65 DiskLab.app'
touch 'dist/MEGA65 DiskLab.app'  # so Finder shows the rebuild date on the bundle
echo 'Built dist/MEGA65 DiskLab.app (local ad-hoc signature).'
