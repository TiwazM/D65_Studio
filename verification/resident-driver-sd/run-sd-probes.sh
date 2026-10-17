#!/bin/sh
# Runs the direct-SD reader through every fixture file and failure mode.
set -e
F=${1:-/home/claude/fixtures}; R=${2:-/home/claude/d65/launcher/reader.bin}; BASE=${3:-0x123456}
n=0
while read i name t s; do
  case $name in
    GAME00) modes="0 1 2 3 4 5 6 7 8 9 10 11 12 13";;
    GAME*) modes="0";;
    MESSAGE) modes="20 21 22 23 24";;
    FILLER) modes="25";;
    *) modes="20 21 22";;
  esac
  for m in $modes; do
    ./sd-probe $F/FULLSIZE.D65 $F/$name.prg $i $m $R $F/catalog.bin $BASE > /tmp/probe.out || { echo "FAIL $name mode $m"; cat /tmp/probe.out; exit 1; }
    n=$((n+1))
  done
done < $F/files.txt
echo "PASS: $n CPU/SD cases with $R base $BASE"
