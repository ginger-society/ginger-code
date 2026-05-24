#!/bin/bash
set -e

# cargo build --release

APP=ginger-code-gui.app
rm -rf $APP
mkdir -p $APP/Contents/MacOS    

cp target/debug/ginger-code-gui $APP/Contents/MacOS/ginger-code-gui
cp Info.gui.plist $APP/Contents/Info.plist

echo "Built $APP GUI"