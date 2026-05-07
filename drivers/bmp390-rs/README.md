# BMI323 Rust Driver

This is a Rust driver for the Bosch BMP390 precision barometer and altimeter.

## Features

- Support for both I2C and SPI interfaces
- Support for both synchronous and asynchronous interfaces
- Configurable sensor settings
- Configurable interrupt settings
- Reading raw and scaled sensor data
- Error handling and device initialization

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
bmp390-rs = "0.0.1"  # Replace with the actual version
```

Here's a basic example of how to use the driver:

```rust
use bmp390_rs::{degree_celsius, foot, hectopascal, meter, AsyncFunctions as _, Bmp390, Bmp390Config, Length};
use defmt::*;
use embedded_hal::blocking::i2c::I2c;

fn main() {
    // Initialize your I2C or SPI interface
    let i2c = // ... initialize your I2C interface
    let delay = // ... initialize your delay provider

    // Create a new BMI323 instance
    let mut baro = Bmp390::new_with_i2c(
        i2c,
        bmp390_rs::DEFAULT_I2C_ADDRESS,
        Bmp390Config::default()
            .enable_irq(true),
        embassy_time::Delay,
    );

    // Initialize the device, and get the calibration coefficients
    let cal = baro.init().unwrap();
    // Start measurement
    baro.start().unwrap();
    // Initialize interrupt pin here, if enabled
    loop {
        // Poll for interrupt if enabled
        // Make measurement
        if let Ok(data) = baro.measure() {
            if let Some((temp, pres, alt)) = cal.convert(data, Length::new::<meter>(0.0)) {
                info!(
                    "Temperature: {}°C, Pressure: {}hPa, Altitude: {}ft",
                    temp.get::<degree_celsius>(),
                    pres.get::<hectopascal>(),
                    alt.get::<foot>()
                );
            }
            info!("Time since last sample: {}ms", elapsed.as_millis());
        }
    }

}
```

## License

This project is licensed under Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0).

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## References

- [BMP390 Product Page](https://www.bosch-sensortec.com/products/environmental-sensors/pressure-sensors/bmp390/)
- [BMP390 Datasheet](https://www.bosch-sensortec.com/media/boschsensortec/downloads/datasheets/bst-bmp390-ds002.pdf)
