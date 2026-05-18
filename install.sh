#!/usr/bin/env bash
set -e
cd "$(dirname "$0")"
cargo build --release
cp target/release/greeting ~/.local/bin/greeting
echo "Installed to ~/.local/bin/greeting"
echo ""
echo "First run:"
echo "  greeting init     # write default config"
echo "  greeting update   # collect data now"
echo "  greeting show     # test output"
echo ""
echo "Add to hyprland.lua:"
echo "  hl.exec_cmd("greeting daemon")"
echo ""
echo "Add to fish config:"
echo "  greeting"
