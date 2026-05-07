<section class="manual-sheet" id="manual-start" markdown="1">

# Description

This document covers the **Kiwi Firmware** — embedded firmware for Kiwi. 
The firmware is written in the [Rust Programming Language](https://rust-lang.org)<span class="cite-ref" data-ref="rust-book"></span> using the [Embassy](https://embassy.dev)<span class="cite-ref" data-ref="embassy"></span> async framework and provides sensor acquisition, Wi-Fi access-point streaming, USB configuration, and hardware watchdog functionality.
It targets Kiwi<span class="cite-ref" data-ref="kiwi-hw"></span> based on the Raspberry Pi RP2350B.

## Key Features

- Async sensor tasks for magnetometer (MMC5983MA), IMU (BMI323), barometer (MS5607), and environmental sensor (BME680)
- Wi-Fi access point/client mode with UDP measurement streaming via `embassy-net`
- USB HID/CDC interface for configuration
- Per-task hardware watchdog with reset-reason logging
- Compile-time sensor selection via Cargo feature flags
- `no_std` sensor driver library crates for reuse in derived projects

> **WARNINGS & Safety Measures**
> Electrical devices connected to Kiwi should not be near any liquids and/or high temperature environments (above 85°C, 185°F) as it may cause internal or external damage to the Kiwi, your device connected to the Kiwi, and/or injury to yourself.
> Kiwi is designed primarily to be powered over USB (5V), consuming around 250 mW of power.
> 
> Kiwi is an exposed PCB, with electronic components sensitive to static discharge.
> Use caution while handling.
{: .callout-warning }

</section>
