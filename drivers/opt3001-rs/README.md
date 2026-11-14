# opt3001-rs

A `no_std` Rust driver for the [Texas Instruments OPT3001](https://www.ti.com/product/OPT3001) ambient light sensor, supporting both synchronous ([`embedded-hal`](https://docs.rs/embedded-hal)) and asynchronous ([`embedded-hal-async`](https://docs.rs/embedded-hal-async)) operation over I2C.

## Features

- I2C interface
- Synchronous (`sync` feature) and asynchronous (`async` feature) APIs
- Configurable range, conversion time, conversion mode, interrupt polarity, latch behavior, exponent masking, and fault count
- Manufacturer and device ID validation during initialization
- Fixed-point milli-lux output by default
- Optional `float` feature for `uom`-typed (`Illuminance`) output
- Optional `defmt` logging support
- `no_std` compatible

## Feature Flags

| Feature          | Description |
|------------------|-------------|
| `sync`           | Enables the blocking `SyncInterface` via `embedded-hal` |
| `async`          | Enables the async `AsyncInterface` via `embedded-hal-async` |
| `float`          | Returns `Measurement` as a `uom` `Illuminance`; without this flag values are raw fixed-point milli-lux |
| `defmt`          | Derives `defmt::Format` for public types |
| `defmt-messages` | Enables `defmt` trace/debug messages inside the driver (implies `defmt`) |

**Default features:** `defmt-messages`, `async`, `sync`

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
opt3001-rs = { version = "0.0.1", features = ["async", "float"] }
```

### Async Example

```rust
use opt3001_rs::{Address, AsyncInterface as _, Config, Opt3001};

let i2c = /* ... */;
let delay = /* ... */;

let mut sensor = Opt3001::new_i2c(i2c, Address::Gnd, delay, Config::default());

sensor.init().await.unwrap();
let measurement = sensor.read().await.unwrap();

// With the `float` feature:
use opt3001_rs::uom::si::illuminance::lux;
let illuminance_lux = measurement.illuminance.get::<lux>();

// Without the `float` feature, measurement.illuminance_mlux is a u32 in milli-lux.
```

### Sync Example

```rust
use opt3001_rs::{Address, Config, Opt3001, SyncInterface as _};

let i2c = /* ... */;
let delay = /* ... */;

let mut sensor = Opt3001::new_i2c(i2c, Address::Gnd, delay, Config::default());

sensor.init().unwrap();
let measurement = sensor.read().unwrap();
```

## I2C Address

The OPT3001 I2C address is set by the ADDR pin:

| `Address` variant | Address | ADDR pin |
|-------------------|---------|----------|
| `Gnd`             | `0x44`  | GND      |
| `Vdd`             | `0x45`  | VDD      |
| `Sda`             | `0x46`  | SDA      |
| `Scl`             | `0x47`  | SCL      |

## Measurement Output

### With `float` feature

`Measurement` fields are `uom::si::f32` quantities:

```rust
pub struct Measurement {
    pub illuminance: Illuminance,
}
```

### Without `float` feature

`Measurement` (`MeasurementRaw`) fields are fixed-point integers:

```rust
pub struct MeasurementRaw {
    pub illuminance_mlux: u32, // milli-lux, e.g. 1234 = 1.234 lux
}
```

## License

Apache-2.0 - see the workspace root for details.
