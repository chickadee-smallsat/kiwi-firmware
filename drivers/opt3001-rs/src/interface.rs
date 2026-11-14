pub trait Interface {}

/// I2C interface for the OPT3001 sensor.
pub struct I2cInterface<I2C> {
    pub(crate) i2c: I2C,
    pub(crate) address: u8,
}

impl<I2C> Interface for I2cInterface<I2C> {}
