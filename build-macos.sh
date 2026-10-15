#!/bin/sh
set -eu
cd "$(dirname "$0")"
cargo build --locked --release --target aarch64-apple-darwin
cargo build --locked --release --target x86_64-apple-darwin
bundle='dist/D65 Studio.app/Contents'
mkdir -p "$bundle/MacOS" "$bundle/Resources"
lipo -create target/aarch64-apple-darwin/release/d65-studio target/x86_64-apple-darwin/release/d65-studio -output "$bundle/MacOS/d65-studio"
cp README.md FORMAT.md launcher/Reader-notes.txt launcher/Resident-driver.txt launcher/Resident-demo.bas launcher/LOAD-compatibility.txt "$bundle/Resources/"
cp packaging/Info.plist "$bundle/Info.plist"
codesign --force --deep --sign - 'dist/D65 Studio.app'
echo 'Built dist/D65 Studio.app (local ad-hoc signature).'
