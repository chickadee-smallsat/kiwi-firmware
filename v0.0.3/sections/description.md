<section class="manual-sheet" id="manual-start" markdown="1">

# Description

This document covers the **Kiwi Firmware** — embedded firmware for Kiwi. 
The firmware is written in the [Rust Programming Language](https://rust-lang.org)<span class="cite-ref" data-ref="rust-book"></span> using the [Embassy](https://embassy.dev)<span class="cite-ref" data-ref="embassy"></span> async framework and provides sensor acquisition, measurement streaming over Wi-Fi or Bluetooth Low-Energy (BLE), microSD logging, USB configuration, and hardware watchdog functionality.
It targets Kiwi<span class="cite-ref" data-ref="kiwi-hw"></span> based on the Raspberry Pi RP2350B.

## Key Features

- Async sensor tasks for the magnetometer (MMC5983MA), IMU (BMI323), barometer (MS5607, or BMP390 on older boards), environmental sensor (BME680), and two ambient light sensors (OPT3001, top and bottom)
- Measurement streaming over Wi-Fi (UDP broadcast) as an access point or a network client, falling back to an access point if the configured network cannot be joined
- Measurement streaming over BLE to up to four clients at once, with no pairing required
- The `BOOTSEL` button switches between Wi-Fi and BLE at any time
- Measurement logging to a microSD card
- USB serial interface for configuration, with firmware logs forwarded over the same USB connection
- Per-task hardware watchdog with reset-reason logging
- Optional dual-core operation and CPU-usage reporting
- Compile-time selection of sensors, default radio mode, and more via Cargo feature flags
- `no_std` sensor driver library crates for reuse in derived projects

> **WARNINGS & Safety Measures**
> Electrical devices connected to Kiwi should not be near any liquids and/or high temperature environments (above 85°C, 185°F) as it may cause internal or external damage to the Kiwi, your device connected to the Kiwi, and/or injury to yourself.
> Kiwi is designed primarily to be powered over USB (5V), consuming around 250 mW of power.
> 
> Kiwi is an exposed PCB, with electronic components sensitive to static discharge.
> Use caution while handling.
{: .callout-warning }

</section>
