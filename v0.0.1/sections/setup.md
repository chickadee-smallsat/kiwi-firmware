<section class="manual-sheet" id="setup" markdown="1">

# Setup

## Linux and macOS

Run the setup script from the repository root. It will install `rustup`, the Rust stable toolchain, the RP2350 cross-compilation target, and `probe-rs`. On Linux it also installs udev rules (requires `sudo`).

```sh
./setup.sh
```

After the script completes, start a new shell session (or source the Cargo environment) to make the new tools available on `PATH`:

```sh
source ~/.cargo/env
```

**Linux only — udev rules:**  
The script installs udev rules automatically. If you need to do this manually:

```sh
sudo install -m 644 rules/*.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules && sudo udevadm trigger
```

## Windows

Open **PowerShell as Administrator** and run the setup script from the repository root:

```powershell
.\setup.ps1
```

The script will:

1. Install (or update) the **MSVC C++ Build Tools** with the _Desktop development with C++_ workload
2. Install `rustup` and the Rust stable toolchain
3. Add the `thumbv8m.main-none-eabihf` cross-compilation target
4. Install `probe-rs`

Close the PowerShell window and open a new one before building, so that all updated `PATH` entries take effect.

## Verifying the Installation

After setup, confirm the key tools are available:

Linux / macOS:
```console
rustc --version          # e.g. rustc 1.87.0 (...)
cargo --version          # e.g. cargo 1.87.0 (...)
probe-rs --version       # e.g. probe-rs 0.27.0
rustup target list --installed | grep thumbv8m
```

Windows:
```powershell
rustc --version          # e.g. rustc 1.87.0 (...)
cargo --version          # e.g. cargo 1.87.0 (...)
probe-rs --version       # e.g. probe-rs 0.27.0
rustup target list --installed | Select-String thumbv8m
```

## Building and Flashing

Connect your debug probe to the Kiwi, then run:

```console
cargo run --release
```

This builds the firmware in release mode and flashes it via `probe-rs`. RTT/defmt log output is streamed to the terminal automatically.

To enable specific sensor subsystems, pass the relevant feature flags:

```console
cargo run --release --features sensors-all
```

See the [firmware Cargo.toml]({{ site.github.repository_url }}/blob/master/firmware/Cargo.toml) for the full list of available features.

## Attaching to a Running Target

To attach `probe-rs` to already-running firmware without reflashing:

Linux / macOS:
```sh
probe-rs attach target/thumbv8m.main-none-eabihf/release/kiwi-firmware-base
```

Windows:
```powershell
probe-rs attach target\thumbv8m.main-none-eabihf\release\kiwi-firmware-base
```

## Panic: WiFi firmware verification failed

At startup, the firmware reads the CYW43439 Wi-Fi firmware blob and control logic and management blob from dedicated regions of the Kiwi's on-board memory and verifies each with a CRC-16 checksum.
This panic means the data at those addresses did not match the checksum baked in at compile time.

#### Possible causes

- The Wi-Fi firmware and CLM blobs have never been flashed to the device.
- The main firmware was rebuilt (changing the flash layout) without re-flashing the WiFi blobs.
- The blobs were flashed to the wrong addresses.

#### Fix

Build the project once so the helper flash script is generated, then run it **before** flashing the main firmware:

```console
# Build (generates the script)
cargo build --release
```

On Linux / macOS — flash WiFi FW, CLM, and credentials

```sh
./firmware/flash-wifi-fw.sh
```

On Windows:

```console
.\firmware\flash-wifi-fw.bat
```

The script calls `probe-rs download` three times, writing the WiFi firmware, CLM blob, and device-credentials page to the addresses computed by `build.rs`. After the script completes, flash the main firmware as usual:

```console
cargo run --release
```

</section>
