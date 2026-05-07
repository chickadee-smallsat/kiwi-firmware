# ms5607-rs

A `no_std` Rust driver for the [TE Connectivity MS5607](https://www.te.com/usa-en/product-CAT-BLPS0035.html) barometric pressure and temperature sensor, supporting both synchronous ([`embedded-hal`](https://docs.rs/embedded-hal)) and asynchronous ([`embedded-hal-async`](https://docs.rs/embedded-hal-async)) operation over I²C or SPI.

## Features

- I²C and SPI interfaces
- Synchronous (`sync` feature) and asynchronous (`async` feature) APIs
- Configurable oversampling ratio (256 to 4096 samples)
- Full on-chip compensation with second-order temperature correction
- PROM CRC-4 integrity check on factory calibration data
- Optional `float` feature for `uom`-typed (`Pressure`, `ThermodynamicTemperature`) output
- Optional `defmt` logging support
- `no_std` compatible

## Sensor Overview

| Parameter   | Range / Resolution                                 |
|------------|---------------------------------------------------|
| Pressure    | 1 … 200 mbar (10 … 1200 mbar absolute)           |
| Temperature | –40 … +85 °C                                     |
| Interface   | I²C (up to 400 kHz) or SPI (mode 0 or 3)         |
| Resolution  | 24-bit ADC output, 6 factory calibration coefficients |

## Feature Flags

| Feature           | Description                                                                 |
|-------------------|-----------------------------------------------------------------------------|
| `sync`            | Enables the blocking `SyncInterface` via `embedded-hal`                     |
| `async`           | Enables the async `AsyncInterface` via `embedded-hal-async`                 |
| `float`           | Returns `Measurement` as `uom` quantities (`Pressure`, `ThermodynamicTemperature`); without this flag values are raw fixed-point `i32` (hundredths of mbar / °C) |
| `defmt`           | Derives `defmt::Format` for public types                                    |
| `defmt-messages`  | Enables `defmt` trace/debug messages inside the driver (implies `defmt`)    |

**Default features:** `defmt-messages`, `async`, `sync`

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
ms5607-rs = { version = "0.0.1", features = ["async", "float"] }
```

### Async Example (I²C)

```rust
use ms5607_rs::{Ms5607, Address, Oversampling, AsyncInterface as _};

let i2c   = /* ... */;
let delay = /* ... */;

let mut sensor = Ms5607::new_i2c(i2c, Address::CsbHigh, delay, Oversampling::Osr4096);

// Reset and read PROM calibration data (CRC-4 verified).
sensor.init().await.unwrap();

// Take a compensated measurement.
let m = sensor.read().await.unwrap();

// With the `float` feature:
use ms5607_rs::uom::si::{pressure::millibar, thermodynamic_temperature::degree_celsius};
let pres_mb = m.pressure.get::<millibar>();
let temp_c  = m.temperature.get::<degree_celsius>();

// Without the `float` feature, m.pressure and m.temperature are i32
// in hundredths of mbar and hundredths of °C respectively.
```

### Sync Example (SPI)

```rust
use ms5607_rs::{Ms5607, Oversampling, SyncInterface as _};

let spi   = /* ... */;
let delay = /* ... */;

let mut sensor = Ms5607::new_spi(spi, delay, Oversampling::Osr2048);

sensor.init().unwrap();

let m = sensor.read().unwrap();
```

## I²C Address

The MS5607 I²C address is set by the CSB pin:

| `Address` variant | Address | CSB pin |
|-------------------|---------|---------|
| `CsbHigh`         | `0x76`  | VDD     |
| `CsbLow`          | `0x77`  | GND     |

## Oversampling Options

| Variant      | Samples | Conversion time |
|-------------|---------|----------------|
| `Osr256`    | 256     | 0.6 ms         |
| `Osr512`    | 512     | 1.17 ms        |
| `Osr1024`   | 1024    | 2.28 ms        |
| `Osr2048`   | 2048    | 4.54 ms        |
| `Osr4096`   | 4096    | 9.04 ms *(default)* |

Higher oversampling reduces noise at the cost of a longer conversion time. The driver waits the required conversion time automatically.

## Measurement Output

### With `float` feature

`Measurement` fields are `uom::si::f32` quantities:

```rust
pub struct Measurement {
    pub temperature: ThermodynamicTemperature, // degree_celsius
    pub pressure: Pressure,                    // millibar
}
```

### Without `float` feature

`Measurement` (`MeasurementRaw`) fields are fixed-point `i32`:

```rust
pub struct MeasurementRaw {
    pub temperature: i32, // hundredths of a degree Celsius (e.g. 2000 = 20.00 °C)
    pub pressure: i32,    // hundredths of a millibar (e.g. 110002 = 1100.02 mbar)
}
```

## Error Handling

`Error<E>`:

| Variant          | Meaning                                                  |
|-----------------|----------------------------------------------------------|
| `Comm(E)`        | Underlying I²C / SPI communication error                 |
| `Crc`            | PROM CRC-4 check failed (corrupted calibration data)     |
| `NotInitialized` | `read()` was called before `init()` / `reset()`          |

## API Reference

Both `SyncInterface` and `AsyncInterface` expose the same three methods:

| Method    | Description                                                         |
|----------|---------------------------------------------------------------------|
| `init`   | Alias for `reset` — soft-reset then reads PROM calibration          |
| `reset`  | Soft-reset the device and re-read factory calibration from PROM     |
| `read`   | Trigger pressure + temperature conversion and return compensated data |

## License

Apache-2.0 — see the workspace root for details.
