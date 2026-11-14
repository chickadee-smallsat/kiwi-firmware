<section class="manual-sheet" id="prerequisites" markdown="1">

# Prerequisites

## Supported Platforms

The tools required to compile the Kiwi firmware runs on the following host operating systems:

| Platform | Minimum Version | Notes |
|----------|----------------|-------|
| **Linux** (x86-64 / ARM64) | Any modern distribution (Ubuntu 20.04+, Fedora 38+, Arch, etc.) | Recommended platform |
| **macOS** (x86-64 / Apple Silicon) | macOS 12 Monterey or later | Same setup script as Linux |
| **Windows** (x86-64) | Windows 10 (21H2) or later | Requires MSVC C++ Build Tools |

> **Note:** 32-bit host systems are not supported.

## Required Hardware

- **Kiwi** (based on the [Raspberry Pi RP2350B](https://pip-assets.raspberrypi.com/categories/1214-rp2350/documents/RP-008373-DS-2-rp2350-datasheet.pdf))
- A **USB cable** to connect Kiwi to your computer
- *Optional:* a supported **debug probe** — any probe compatible with `probe-rs` (e.g. [Raspberry Pi Debug Probe](https://www.raspberrypi.com/documentation/microcontrollers/debug-probe.html), J-Link, CMSIS-DAP), and a cable to connect it to Kiwi (neither provided). Using the Raspberry Pi Debug Probe is recommended to access the on-board [ARM Serial Wire Debugging interface](https://developer.arm.com/documentation/ihi0031/a/The-Serial-Wire-Debug-Port--SW-DP-/Introduction-to-the-ARM-Serial-Wire-Debug--SWD--protocol).

> Flashing and log streaming work over the USB cable alone (see [Building and Flashing](#building-and-flashing)). A debug probe adds on-chip debugging (breakpoints, stepping, inspecting variables), and lets you recover a board whose firmware no longer brings up USB.
{: .callout-note }

## Software Dependencies Installed by the Setup Script

The setup scripts (`setup.sh` / `setup.ps1`) install the following tools automatically:

| Tool | Purpose |
|------|---------|
| [Rust](https://www.rust-lang.org/) (stable) via `rustup` | Compiler and package manager |
| `thumbv8m.main-none-eabihf` target | Cross-compilation target for the RP2350B (Cortex-M33) |
| [`probe-rs`](https://probe.rs/) | Flashing and RTT/defmt log streaming over the debug probe |
| MSVC C++ Build Tools *(Windows only)* | Linker required by the Rust toolchain on Windows |

The default USB flashing tool, [`probe-rp-usb`](https://github.com/sunipkm/probe-rp-usb), is not installed by the setup scripts.
Install it with `cargo install probe-rp-usb` (see [Building and Flashing](#building-and-flashing)).

### Additional Linux Step

On Linux, the setup script also installs **udev rules** (requiring `sudo`) so that the debug probe and Kiwi board are accessible to the current user without root privileges.

</section>
