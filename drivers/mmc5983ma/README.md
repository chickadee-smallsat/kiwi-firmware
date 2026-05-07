# mmc5983ma

A `no_std` Rust driver for the [MEMSIC MMC5983MA](https://www.memsic.com/magnetometer-5.html) 3-axis magnetometer, supporting both synchronous ([`embedded-hal`](https://docs.rs/embedded-hal)) and asynchronous ([`embedded-hal-async`](https://docs.rs/embedded-hal-async)) operation over I²C or SPI.

## Features

- I²C and SPI interfaces
- Synchronous (`sync` feature) and asynchronous (`async` feature) APIs
- Continuous measurement mode with configurable output rate (1–1000 Hz)
- Configurable decimation filter bandwidth (100–800 Hz)
- Periodic SET operation to cancel offset drift
- Per-axis inhibit to disable individual axes
- Measurement-done interrupt (DRDY) pin support
- Bridge-offset cancellation via SET/RST
- Optional `float` feature for `uom`-typed (`MagneticFluxDensity`, `ThermodynamicTemperature`) output
- Optional `defmt` logging support

## Sensor Overview

| Parameter        | Range / Resolution                                   |
|-----------------|-----------------------------------------------------|
| Magnetic range   | ±8 Gauss full-scale                                 |
| Resolution       | 18-bit per axis (~0.25 mG LSB)                     |
| Output rate      | 1 / 10 / 20 / 50 / 100 / 200 / 1000 Hz (continuous) |
| Temperature      | –75 to +125 °C, 0.8 °C/LSB                         |
| I²C address      | `0x30` (fixed)                                       |

## Feature Flags

| Feature           | Description                                                         |
|-------------------|---------------------------------------------------------------------|
| `sync`            | Enables `Mmc5983Sync` and the blocking `embedded-hal` interface     |
| `async`           | Enables `Mmc5983Async` and the `embedded-hal-async` interface       |
| `float`           | Returns `MagneticFluxDensity` / `ThermodynamicTemperature` via `uom`; without this flag, axes are returned as raw `i32` counts |
| `defmt`           | Derives `defmt::Format` for public types                            |
| `defmt-messages`  | Enables `defmt` trace/debug messages inside the driver (implies `defmt`) |

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
mmc5983ma = { version = "0.0.0", features = ["async", "float"] }
```

### Configuration

Use `Mmc5983ConfigBuilder` to create a `Mmc5983Config`. The builder validates that the chosen bandwidth is compatible with the output rate.

```rust
use mmc5983ma::{
    Mmc5983ConfigBuilder, ContinuousMeasurementFreq, DecimationBw,
    PeriodicSetInterval, AxisInhibit,
};

let config = Mmc5983ConfigBuilder::default()
    .frequency(ContinuousMeasurementFreq::Hz10)
    .bandwidth(DecimationBw::Hz100)
    .set_interval(PeriodicSetInterval::Per100)
    .inhibit(AxisInhibit::None)
    .irq(true)
    .build();
```

**Default configuration** (`Mmc5983ConfigBuilder::default()`):

| Field            | Default         |
|-----------------|----------------|
| `frequency`      | `Hz1`           |
| `bandwidth`      | `Hz100`         |
| `set_interval`   | `Per100`        |
| `inhibit`        | `None` (all axes active) |
| `irq`            | `true`          |

> **Note:** When `frequency` ≥ 200 Hz, the builder automatically raises the bandwidth to at least `Hz200`; at 1000 Hz it is forced to `Hz800`.

### Async Example

```rust
use mmc5983ma::{Mmc5983Async, Mmc5983ConfigBuilder, DEFAULT_I2C_ADDRESS};
use mmc5983ma::uom::si::magnetic_flux_density::gauss;

// Obtain an I2C bus, delay, and mutex from your HAL/executor.
let i2c   = /* ... */;
let delay = /* ... */;

let config = Mmc5983ConfigBuilder::default().build();

let mut mag = Mmc5983Async::new_with_i2c(i2c, DEFAULT_I2C_ADDRESS, config, delay);

// Reset the device and apply the configuration.
mag.reset().await.unwrap();

// Read calibrated bridge offset (SET/RST).
mag.calibrate(4).await.unwrap();

// Take a measurement.
let m = mag.measure().await.unwrap();
let (mx, my, mz) = m.milligauss(); // (f32, f32, f32) in mG
```

### Sync Example

```rust
use mmc5983ma::{Mmc5983Sync, Mmc5983ConfigBuilder, DEFAULT_I2C_ADDRESS};

let i2c   = /* ... */;
let delay = /* ... */;

let config = Mmc5983ConfigBuilder::default().build();

let mut mag = Mmc5983Sync::new_with_i2c(i2c, DEFAULT_I2C_ADDRESS, config, delay);

mag.reset().unwrap();
mag.calibrate(4).unwrap();

let m = mag.measure().unwrap();
let (mx, my, mz) = m.milligauss();
```

### SPI

Both `Mmc5983Sync` and `Mmc5983Async` have a `new_with_spi` constructor that accepts an `embedded-hal` / `embedded-hal-async` `SpiDevice`.

```rust
let mut mag = Mmc5983Async::new_with_spi(spi, config, delay);
```

## Output Types

### With `float` feature (default recommendation)

`MagMeasurement` fields are `uom::si::f32::MagneticFluxDensity`:

```rust
let m = mag.measure().await.unwrap();
// Access as milliGauss tuple:
let (x_mg, y_mg, z_mg) = m.milligauss();
// Or use uom quantities directly:
let x_gauss = m.x.get::<gauss>();
```

Temperature (if read separately) is `uom::si::f32::ThermodynamicTemperature`.

### Without `float` feature

`MagMeasurement` fields are raw `i32` counts (zero-centered around the 18-bit midpoint `1 << 17`).

## Continuous Measurement Frequencies

`ContinuousMeasurementFreq`: `Off`, `Hz1`, `Hz10`, `Hz20`, `Hz50`, `Hz100`, `Hz200`, `Hz1000`

## Decimation Bandwidth

`DecimationBw`: `Hz100`, `Hz200`, `Hz400`, `Hz800`

Bandwidth sets the noise floor / measurement time trade-off. Lower bandwidth → lower noise, higher latency.

## Periodic SET Interval

`PeriodicSetInterval`: `Per1`, `Per25`, `Per75`, `Per100`, `Per250`, `Per500`, `Per1000`, `Per2000`, `Off`

A periodic SET pulse re-magnetises the sense element, compensating for slowly drifting bridge offset. `Off` disables automatic SET.

## Error Handling

`Mmc5983Error<E>`:

| Variant          | Meaning                                      |
|-----------------|----------------------------------------------|
| `Comm(E)`        | Underlying I²C / SPI communication error     |
| `InvalidDevice`  | Product-ID register did not match `0x30`     |
| `NotReady`       | Measurement data not ready after polling     |
| `InvalidConfig`  | Requested configuration is not supported     |
| `InvalidAccess`  | Operation not valid in the current state     |

## I²C Address

The MMC5983MA has a fixed I²C address. Use the exported constant:

```rust
use mmc5983ma::DEFAULT_I2C_ADDRESS; // 0x30
```

## License

MIT — see [`LICENSE`](../../LICENSE) or the workspace root for details.
