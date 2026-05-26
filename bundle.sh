#!/bin/bash
set -e

cargo build --release

APP=ginger-code.app
rm -rf $APP
mkdir -p $APP/Contents/MacOS

cp target/release/ginger-code $APP/Contents/MacOS/ginger-code
cp target/release/ginger-code-cli $APP/Contents/MacOS/ginger-code-cli
cp Info.plist $APP/Contents/

echo "Built $APP"