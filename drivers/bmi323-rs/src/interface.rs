pub trait Interface {}

/// I2C interface for the BMI323 sensor.
pub struct I2cInterface<I2C> {
    pub(crate) i2c: I2C,
    pub(crate) address: u8,
}

/// SPI interface for the BMI323 sensor.
pub struct SpiInterface<SPI> {
    pub(crate) spi: SPI,
}

impl<I2C> Interface for I2cInterface<I2C> {}
impl<SPI> Interface for SpiInterface<SPI> {}
