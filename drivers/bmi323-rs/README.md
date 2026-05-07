# BMI323 Rust Driver

This is a Rust driver for the Bosch BMI323 Inertial Measurement Unit (IMU). The BMI323 is a highly integrated, low power IMU that provides precise acceleration and angular rate measurements.

## Features

- Support for both I2C and SPI interfaces
- Support for both synchronous and asynchronous interfaces
- Configurable accelerometer and gyroscope settings
- Configurable interrupt settings
- Reading raw and scaled sensor data
- Error handling and device initialization
- Calibration of gyroscope

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
bmi323 = "0.0.1"  # Replace with the actual version
```

Here's a basic example of how to use the driver with the [embassy](https://embassy.dev/) asynchronous runtime:

```rust
use bmi323::{Bmi323Async, Bmi323Config, AccelMode, AccelRange, GyroMode, GyroRange};
use embedded_sync::blocking_mutex::raw::NoopRawMutex;
use embedded_hal_async::i2c::I2c;

fn main() {
    // Initialize your I2C or SPI interface
    let i2c = // ... initialize your I2C interface
    let delay = // ... initialize your delay provider

    // Create a new BMI323 instance
    let mut imu = Bmi323Async::<_, _, NoopRawMutex>::new_with_i2c(
        i2c,
        bmi323::DEFAULT_I2C_ADDRESS,
        Bmi323Config::default()
            .with_accel_mode(AccelMode::HighPerformance) // Turn on the accelerometer in high performance mode
            .with_gyro_mode(GyroMode::HighPerformance) // Turn on the gyroscope in high performance mode
            .with_gyro_range(GyroRange::Dps250) // Set the gyroscope measurement range
            // .with_acc_irq(IrqMap::Int1) // Set accelerometer to signal data ready on INT1
        delay,
    );

    // Initialize the device
    imu.init().await.unwrap();
    // Calibrate the gyroscope and accelerometer
    imu.calibrate(bmi323_rs::SelfCalibrateType::Both)
        .await
        .unwrap();
    // Start continuous measurement
    imu.start(|imu| async {
        loop {
            // Wait for data ready interrupt or poll for new data
            if let Ok(data) = imu.measure().await {
                // Process the measurement data
            }
        }
        Ok(())
    })
    .await
    .unwrap();
}
```

## License

This project is licensed under Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0).

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## References

- [BMI323 Product Page](https://www.bosch-sensortec.com/products/motion-sensors/imus/bmi323/)
- [BMI323 Datasheet](https://www.bosch-sensortec.com/media/boschsensortec/downloads/datasheets/bst-bmi323-ds000.pdf)
