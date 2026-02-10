#!/bin/bash
# Downloads Mozc OSS dictionary data for use with Lexime.
#
# Mozc is licensed under the BSD 3-Clause License.
# Copyright 2010-2018, Google Inc.
# https://github.com/google/mozc/blob/master/LICENSE
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
OUTPUT_DIR="$PROJECT_DIR/engine/data/mozc-raw"

MOZC_BASE_URL="https://raw.githubusercontent.com/google/mozc/master/src/data/dictionary_oss"
MOZC_LICENSE_URL="https://raw.githubusercontent.com/google/mozc/master/LICENSE"

DICT_FILES=(
    dictionary00.txt
    dictionary01.txt
    dictionary02.txt
    dictionary03.txt
    dictionary04.txt
    dictionary05.txt
    dictionary06.txt
    dictionary07.txt
    dictionary08.txt
    dictionary09.txt
)

ID_DEF_URL="https://raw.githubusercontent.com/google/mozc/master/src/data/dictionary_oss/id.def"

mkdir -p "$OUTPUT_DIR"

echo "Downloading Mozc dictionary files to $OUTPUT_DIR..."

for file in "${DICT_FILES[@]}"; do
    dest="$OUTPUT_DIR/$file"
    if [ -f "$dest" ]; then
        echo "  $file (already exists, skipping)"
        continue
    fi
    echo "  $file"
    curl -fsSL "$MOZC_BASE_URL/$file" -o "$dest"
done

# Download id.def
dest="$OUTPUT_DIR/id.def"
if [ -f "$dest" ]; then
    echo "  id.def (already exists, skipping)"
else
    echo "  id.def"
    curl -fsSL "$ID_DEF_URL" -o "$dest"
fi

# Download LICENSE
dest="$OUTPUT_DIR/LICENSE"
if [ -f "$dest" ]; then
    echo "  LICENSE (already exists, skipping)"
else
    echo "  LICENSE"
    curl -fsSL "$MOZC_LICENSE_URL" -o "$dest"
fi

touch "$OUTPUT_DIR/.stamp"
echo "Done. Files saved to $OUTPUT_DIR"
