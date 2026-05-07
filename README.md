# Kiwi Firmware

Embedded firmware for **Kiwi** (RP2350B), written in Rust using the [Embassy](https://embassy.dev) async framework. The repository is a Cargo workspace containing the main firmware crate and a collection of `no_std` sensor drivers.

## Repository Layout

```
firmware/          Main firmware binary (kiwi-firmware-base)
device-blob/       Flash storage for device config and firmware blobs
drivers/
  bme680-rs/       BME680 environmental sensor (temp / pressure / humidity / gas)
  bmi323-rs/       BMI323 6-axis IMU (accelerometer + gyroscope)
  bmp390-rs/       BMP390 precision barometer
  mmc5983ma/       MMC5983MA 3-axis magnetometer
  ms5607-rs/       MS5607 barometric pressure sensor
config/            Linker memory map template
rules/             Linux udev rules for probe and device access
```

## Firmware Overview

The firmware runs on the RP2350B microcontroller and provides:

- **Sensor acquisition** — reads from up to four sensor subsystems (magnetometer, IMU, barometer, humidity/gas) over a shared I²C bus, each running as an independent Embassy task
- **Wi-Fi Access Point** — hosts a WPA2/open AP (`kiwi-ap` by default) and streams measurements over UDP using `embassy-net`
- **USB interface** — exposes a USB HID/CDC device for configuration and data-rate control
- **Watchdog** — per-task watchdog via `embassy-task-watchdog`; logs reset reason on startup

Sensor subsystems are enabled at compile time via Cargo features (see below).

## Getting Started

### Linux

1. Run `setup.sh` to install Rust, `probe-rs`, and all required targets:
   ```sh
   ./setup.sh
   ```
2. Install udev rules so the probe and device are accessible without root:
   ```sh
   sudo install rules/*.rules /etc/udev/rules.d
   sudo udevadm control --reload-rules && sudo udevadm trigger
   ```
3. Connect a debug probe, then build and flash:
   ```sh
   cargo run --release
   ```

### Windows

1. Run `setup.ps1` in an elevated PowerShell session:
   ```powershell
   .\setup.ps1
   ```
2. Build and flash:
   ```cmd
   cargo run --release
   ```

## Sensor Feature Flags

Enable sensor subsystems by passing features to Cargo:

| Feature        | Sensor                         |
|---------------|-------------------------------|
| `sensor-mag`   | MMC5983MA magnetometer         |
| `sensor-imu`   | BMI323 accelerometer/gyroscope |
| `sensor-baro`  | BMP390 barometer               |
| `sensor-humi`  | BME680 temp/humidity/gas       |
| `sensors-all`  | All four of the above          |

Example — build with all sensors:
```sh
cargo build --release --features sensors-all
```

IMU output data rates can be fine-tuned with additional features such as `imu-accel-odr-100hz` and `imu-gyro-odr-50hz`. See `firmware/Cargo.toml` for the full list.

## Debugging with `probe-rs`

Attach to a running target (RTT + defmt logging):
```sh
# Linux
probe-rs attach target/thumbv8m.main-none-eabihf/release/kiwi-firmware-base

# Windows
probe-rs attach target\thumbv8m.main-none-eabihf\release\kiwi-firmware-base
```

## Drivers

Each driver lives in `drivers/` and has its own README:

| Crate | Sensor | Interfaces | Modes |
|-------|--------|-----------|-------|
| [`bme680-rs`](drivers/bme680-rs/README.md) | Bosch BME680 | I²C | async |
| [`bmi323-rs`](drivers/bmi323-rs/README.md) | Bosch BMI323 | I²C, SPI | sync, async |
| [`bmp390-rs`](drivers/bmp390-rs/README.md) | Bosch BMP390 | I²C, SPI | sync, async |
| [`mmc5983ma`](drivers/mmc5983ma/README.md) | MEMSIC MMC5983MA | I²C, SPI | sync, async |
| [`ms5607-rs`](drivers/ms5607-rs/README.md) | TE MS5607 | I²C, SPI | sync, async |

## Resources

- [Embassy documentation](https://embassy.dev/book/)
- [Embassy IRQ for RP2040/RP2350](https://www.reddit.com/r/rust/comments/1haqrtz/embassy_rs_interrupts_for_the_rp2040/)
- `embassy-rp` commit `5a19b64` adds over/underclocking support for RP-series MCUs