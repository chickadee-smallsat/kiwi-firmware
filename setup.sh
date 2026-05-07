#!/usr/bin/env bash
# Setup script for kiwi-firmware on Linux / macOS
# Installs the Rust toolchain, the RP2350 target, probe-rs, and (Linux) udev rules

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# --- Rust ---
if command -v rustup &>/dev/null; then
    echo "rustup already installed ? updating..."
    rustup update stable
else
    echo "Installing Rust via rustup..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain stable
    # Make cargo/rustup available in this shell session
    # shellcheck source=/dev/null
    source "$HOME/.cargo/env"
fi

# --- Target for RP2350 ---
echo "Adding thumbv8m.main-none-eabihf target..."
rustup target add thumbv8m.main-none-eabihf

# --- probe-rs ---
echo "Installing probe-rs..."
curl --proto '=https' --tlsv1.2 -LsSf \
    https://github.com/probe-rs/probe-rs/releases/latest/download/probe-rs-tools-installer.sh \
    | sh

# --- udev rules (Linux only) ---
if [[ "$(uname -s)" == "Linux" ]]; then
    echo "Installing udev rules..."
    sudo install -m 644 "$SCRIPT_DIR/rules/"*.rules /etc/udev/rules.d/
    sudo udevadm control --reload-rules
    sudo udevadm trigger
    echo "udev rules installed."
fi

echo ""
echo "Setup complete."
echo "  Start a new shell session (or run: source ~/.cargo/env) to pick up PATH changes."
echo "  Connect your debug probe and run:  cargo run --release"
