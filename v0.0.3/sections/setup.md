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

By default, `cargo run` flashes Kiwi over its USB connection with [`probe-rp-usb`](https://github.com/sunipkm/probe-rp-usb).
No debug probe is needed.
Install it once:

Linux (the USB serial support needs libudev):
```sh
sudo apt install libudev-dev pkg-config   # Debian/Ubuntu; use your distribution's equivalent
cargo install probe-rp-usb
```

macOS / Windows:
```console
cargo install probe-rp-usb
```

Connect Kiwi to your computer over USB, then run:

```console
cargo run --release
```

This builds the firmware in release mode, resets the running Kiwi into its `BOOTSEL` bootloader over USB, flashes the new firmware, and then streams the firmware's defmt log output from the USB serial port to the terminal. If Kiwi is not running firmware that answers the USB reset request (for example, a new board), hold `BOOTSEL` while plugging it in first.

### Using a Debug Probe Instead

To flash and stream logs through a `probe-rs` debug probe instead, switch the runner in `.cargo/config.toml`: comment out the `probe-rp-usb` runner line and uncomment the `probe-rs` one:

```toml
# runner = "probe-rp-usb run --family rp2xxx-absolute"
runner = "probe-rs run --chip RP235x --protocol swd"
```

## Build Options

Firmware features are selected with Cargo feature flags.

The default build enables all current sensors (`sensors-all`), dual-core operation, and 50 Hz IMU data rates. Commonly used flags:

| Feature | Effect |
|---------|--------|
| `sensor-mag`, `sensor-imu`, `sensor-baro`, `sensor-humi`, `sensor-light` | Individual sensor subsystems (magnetometer, IMU, MS5607 barometer, BME680, OPT3001 light sensors). `sensors-all` enables all of them. |
| `sensor-old-baro` | BMP390 barometer on older Kiwi boards. Overrides `sensor-baro`, so it can be added on top of the defaults. |
| `ble-default` | Boot in Bluetooth LE mode instead of Wi-Fi (see [Radio Modes](#radio-modes)). |
| `monolithic` | Bake the Wi-Fi/Bluetooth chip firmware blobs into the main image. See [Single-Image Firmware](#single-image-firmware). |
| `imu-accel-odr-*`, `imu-gyro-odr-*` | IMU accelerometer/gyroscope output data rate, e.g. `imu-accel-odr-100hz`. |
| `cpuinfo` | Log CPU usage once per second. |

To build without the defaults (for example, to pick sensors individually), add `--no-default-features` and list what you need, keeping `dual-core` and `defmt`:

```console
cargo run --release --no-default-features --features dual-core,defmt,sensor-mag,sensor-imu
```

See the [firmware Cargo.toml]({{ site.github.repository_url }}/blob/master/firmware/Cargo.toml) for the full list.

### Initial Device Configuration

The device identifier and Wi-Fi settings that a freshly set-up Kiwi starts with are taken from environment variables at build time and written to the device by the [Wi-Fi firmware flash script](#fix):

| Variable | Default | Meaning |
|----------|---------|---------|
| `KIWI_DEVICE_ID` | `kiwi#0001` | Device identifier (up to 12 characters); also the Bluetooth device name |
| `KIWI_WIFI_SSID` | `kiwi-ap` | Wi-Fi network name |
| `KIWI_WIFI_PASSWORD` | *(none: open network)* | Wi-Fi password |
| `KIWI_WIFI_WPA2` | *(unset)* | If set, join `KIWI_WIFI_SSID` as a client; otherwise host it as an access point |

These can be changed later without reflashing, over the [serial interface](#serial-commands).

### Log Level

The firmware log level is set at compile time by `DEFMT_LOG` in `.cargo/config.toml` (default `info`).

## Attaching to a Running Target

To watch the logs of already-running firmware without reflashing:

```console
probe-rp-usb watch target/thumbv8m.main-none-eabihf/release/kiwi-firmware-base
```

`watch` reconnects automatically when Kiwi resets; `attach` connects once.
With a debug probe, use `probe-rs attach` instead:

Linux / macOS:
```sh
probe-rs attach target/thumbv8m.main-none-eabihf/release/kiwi-firmware-base
```

Windows:
```powershell
probe-rs attach target\thumbv8m.main-none-eabihf\release\kiwi-firmware-base
```

## Panic: WiFi (or Bluetooth) firmware verification failed

At startup, the firmware reads the CYW43439 Wi-Fi firmware blob, the control logic and management (CLM) blob, and the Bluetooth firmware blob from dedicated regions of the Kiwi's on-board memory and verifies each with a CRC-16 checksum.
This panic means the data at those addresses did not match the checksum baked in at compile time.

#### Possible causes

- The Wi-Fi firmware, CLM, and Bluetooth firmware blobs have never been flashed to the device.
- The device was set up before Bluetooth support was added, so the Bluetooth firmware blob is missing (`Bluetooth firmware verification failed`).
- The main firmware was rebuilt (changing the flash layout) without re-flashing the WiFi blobs.
- The blobs were flashed to the wrong addresses.

#### Fix

Build the project once so the helper flash script is generated, then run it **before** flashing the main firmware:

```console
# Build (generates the script)
cargo build --release
```

On Linux / macOS — flash WiFi FW, CLM, credentials, and Bluetooth FW over USB (with `probe-rp-usb`):

```sh
./firmware/flash-wifi-fw.sh
```

On Windows:

```console
.\firmware\flash-wifi-fw.bat
```

With a debug probe, run `flash-wifi-fw-probe-rs.sh` (or `.bat`) instead.

The script writes the WiFi firmware, CLM blob, device-credentials page, and Bluetooth firmware to the addresses computed by `build.rs`.
The credentials page holds the [initial device configuration](#initial-device-configuration), so running the script also resets the identifier and Wi-Fi settings.
After the script completes, flash the main firmware as usual:

```console
cargo run --release
```

## Single-Image Firmware

The `monolithic` feature bakes the Wi-Fi and Bluetooth chip firmware into the main firmware image, so the separate [Wi-Fi firmware flash script](#fix) is not needed. Build and flash it like any other build:

```console
cargo run --release --features monolithic
```

Flashing a single image leaves the stored [device configuration](#initial-device-configuration) untouched.

## Installing a Prebuilt Release

Each [GitHub release]({{ site.github.repository_url }}/releases/latest) includes a ready-to-flash single-image firmware, `kiwi-firmware.uf2`. Installing it needs no tools:

1. Hold the `BOOTSEL` button (next to the USB port) and connect Kiwi to your computer over USB.
2. Release `BOOTSEL` once Kiwi appears as a USB mass-storage drive.
3. Copy `kiwi-firmware.uf2` to that drive.

Kiwi restarts with the new firmware once the copy completes, and exposes its [serial command interface](#serial-commands) again.

> The release image does not contain a device configuration. A Kiwi that has never had one stored starts as an open access point named `kiwi-ap`, with the identifier `kiwiUF2`. Set its identifier and Wi-Fi settings over the [serial interface](#serial-commands).
{: .callout-note }

</section>
