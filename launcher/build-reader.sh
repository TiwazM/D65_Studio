#!/bin/sh
set -eu
cd "$(dirname "$0")"
"${1:-acme}" -f plain -o reader.bin reader.a
"${1:-acme}" -DCOMPAT=1 -f plain -o reader-compat.bin reader.a
"${1:-acme}" -DFLOPPY=1 -f plain -o reader-floppy.bin reader.a
"${1:-acme}" -f plain -o compat-gateway.bin compat-gateway.a
"${1:-acme}" -f plain -o compat-load.bin compat-load.a
"${1:-acme}" -f plain -o floppywrite.bin floppywrite.a
