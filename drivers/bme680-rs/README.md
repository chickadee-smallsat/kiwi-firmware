# bme680-rs

An async `no_std` Rust driver for the [Bosch BME680](https://www.bosch-sensortec.com/products/environmental-sensors/gas-sensors/bme680/) environmental sensor.

## Features

- Temperature, pressure, humidity, and gas resistance measurements
- Full on-chip compensation (integer math via `libm`-backed `uom` quantities)
- Async-only API using `embedded-hal-async` traits
- Configurable oversampling for temperature, pressure, and humidity
- Configurable gas heater profile (target temperature and on-time)
- Optional `defmt` logging via the `defmt-messages` feature flag
- `no_std` compatible — suitable for bare-metal embedded targets

## Sensor Overview

| Parameter        | Range                | Output type               |
|-----------------|---------------------|--------------------------|
| Temperature      | -40 … +85 °C        | `ThermodynamicTemperature` |
| Pressure         | 300 … 1100 hPa       | `Pressure`                |
| Humidity         | 0 … 100 %rH          | `Ratio`                   |
| Gas resistance   | depends on heater    | `ElectricalResistance`    |

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
bme680-rs = "0.0.1"
```

### Basic Example

```rust
use bme680_rs::{
    Bme680, Bme680Address, Config, Oversampling,
    degree_celsius, pascal, percent, ohm,
};

// Obtain an I2C bus and a delay provider from your HAL.
let i2c = /* ... */;
let delay = /* ... */;

// Construct the driver (address pin tied low → 0x76).
let mut sensor = Bme680::new(i2c, Bme680Address::AddrLow, delay);

// Build a configuration (or use Config::default()).
let config = Config {
    os_temperature: Oversampling::X2,
    os_pressure: Oversampling::X16,
    os_humidity: Oversampling::X1,
    heater_temperature: 320,   // °C
    heater_duration_ms: 100,
    ambient_temperature: 25,   // °C, used for heater resistance calculation
};

// Initialize: soft-reset → chip-ID check → read calibration → apply config.
sensor.init(config).await.unwrap();

// Take a single forced-mode measurement.
let m = sensor.measure().await.unwrap();

let temp_c  = m.temperature.get::<degree_celsius>();
let pres_pa = m.pressure.get::<pascal>();
let hum_pct = m.humidity.get::<percent>();
let gas_ohm = m.gas_resistance.get::<ohm>();
let valid   = m.gas_valid; // heater stable + reading valid
```

## Configuration

### `Config` fields

| Field                | Type  | Default | Description                                          |
|----------------------|-------|---------|------------------------------------------------------|
| `os_temperature`     | `Oversampling` | `X2`  | Temperature oversampling                     |
| `os_pressure`        | `Oversampling` | `X16` | Pressure oversampling                        |
| `os_humidity`        | `Oversampling` | `X1`  | Humidity oversampling                        |
| `heater_temperature` | `u16` | `320`   | Gas heater target temperature in °C (max 400)        |
| `heater_duration_ms` | `u16` | `100`   | Gas heater on-time in milliseconds                   |
| `ambient_temperature`| `i8`  | `25`    | Approximate ambient °C for heater resistance calc    |

`Config::default()` provides sensible defaults for most use cases.

### Oversampling options

`Oversampling::None`, `X1`, `X2`, `X4`, `X8`, `X16`

Disabling a measurement (`None`) skips that sensor and removes its contribution from the measurement duration estimate.

### Measurement duration

`Config::measure_duration_ms()` computes the worst-case measurement time (ms) for the current settings using the formula from the BME680 datasheet §3.2.1. The driver waits this long after triggering forced mode, then polls for the `new_data` flag.

## I²C Address

| `Bme680Address` variant | Address | SDO pin |
|------------------------|---------|---------|
| `AddrLow`              | `0x76`  | GND     |
| `AddrHigh`             | `0x77`  | VDDIO   |

## Feature Flags

| Feature           | Description                                      |
|-------------------|--------------------------------------------------|
| `defmt`           | Derives `defmt::Format` for public types         |
| `defmt-messages`  | Enables `defmt` log messages inside the driver (implies `defmt`) |

## Error Handling

`Error<E>` wraps three variants:

- `Error::I2c(E)` — underlying I²C bus error
- `Error::InvalidChipId` — chip-ID register did not return `0x61`
- `Error::MeasurementTimedOut` — `new_data` flag not set after repeated polling

## License

See the workspace root for license information.
