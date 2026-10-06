#!/usr/bin/env sh
# SPDX-License-Identifier: MIT
# Run in the bounded Alpine container with signed art_standalone-dev and JDK17.
# Input tree must be the unmodified pinned ATL commit; never patches a live runtime.
set -eu
if [ "$#" -ne 2 ]; then echo 'Usage: build_framework.sh PINNED_SOURCE NEW_OUTPUT' >&2; exit 2; fi
source_dir=$(realpath "$1")
case "$2" in /*) output=$2 ;; *) output="$(pwd)/$2" ;; esac
mkdir "$output"
tools_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
python3 "$tools_dir/patch_framework.py" "$source_dir"
mkdir "$output/classes" "$output/headers"
find "$source_dir/src/api-impl" -name '*.java' > "$output/java-sources"
javac -J-Xmx1g -source 8 -target 8 -bootclasspath /usr/lib/java/core-all_classes.jar \
    -h "$output/headers" -d "$output/classes" @"$output/java-sources"
jar cf "$output/hax.jar" -C "$output/classes" .
dx --dex --output="$output/api-impl.jar" "$output/hax.jar"
cc -std=c11 -O2 -Wall -Wextra -Werror -shared -fPIC \
    -I/usr/lib/jvm/java-17-openjdk/include -I/usr/lib/jvm/java-17-openjdk/include/linux \
    "$tools_dir/chooser.c" $(pkg-config --cflags --libs gtk4 gio-2.0) \
    -o "$output/libtilo-android-chooser.so"
sha256sum "$output/api-impl.jar" "$output/libtilo-android-chooser.so"
