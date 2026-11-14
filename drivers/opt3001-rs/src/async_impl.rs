use core::future::Future;

use embedded_hal_async::{delay::DelayNs, i2c};

use crate::{
    Config, Error, Measurement, Mode, Opt3001, Status, debug,
    details::{
        DEVICE_ID, MANUFACTURER_ID, MeasurementRaw, REG_CONFIGURATION, REG_DEVICE_ID,
        REG_MANUFACTURER_ID, REG_RESULT,
    },
    interface::{I2cInterface, Interface},
};

/// Asynchronous I/O interface for the OPT3001 device.
pub trait AsyncIo: Interface {
    /// The error type for asynchronous operations.
    type Error;

    /// Read a big-endian 16-bit register.
    fn read_register(&mut self, register: u8) -> impl Future<Output = Result<u16, Self::Error>>;

    /// Write a big-endian 16-bit register.
    fn write_register(
        &mut self,
        register: u8,
        value: u16,
    ) -> impl Future<Output = Result<(), Self::Error>>;
}

impl<I2C, E> AsyncIo for I2cInterface<I2C>
where
    I2C: i2c::I2c<Error = E>,
{
    type Error = I2C::Error;

    fn read_register(&mut self, register: u8) -> impl Future<Output = Result<u16, Self::Error>> {
        let i2c = &mut self.i2c;
        let address = self.address;
        async move {
            let mut buffer = [0_u8; 2];
            i2c.write_read(address, &[register], &mut buffer).await?;
            Ok(u16::from_be_bytes(buffer))
        }
    }

    fn write_register(
        &mut self,
        register: u8,
        value: u16,
    ) -> impl Future<Output = Result<(), Self::Error>> {
        let i2c = &mut self.i2c;
        let address = self.address;
        async move {
            let data = value.to_be_bytes();
            i2c.write(address, &[register, data[0], data[1]]).await
        }
    }
}

/// Asynchronous interface for the OPT3001 device.
pub trait AsyncInterface<IFACE: AsyncIo> {
    /// Initialize the device by validating ID registers and writing the cached configuration.
    fn init(&mut self) -> impl Future<Output = Result<(), Error<IFACE::Error>>>;
    /// Re-write the cached configuration to the device.
    fn reset(&mut self) -> impl Future<Output = Result<(), Error<IFACE::Error>>>;
    /// Update the configuration register and cache the new configuration.
    fn set_config(
        &mut self,
        config: Config,
    ) -> impl Future<Output = Result<(), Error<IFACE::Error>>>;
    /// Read the current status bits from the configuration register.
    fn status(&mut self) -> impl Future<Output = Result<Status, Error<IFACE::Error>>>;
    /// Read an ambient light measurement.
    fn read(&mut self) -> impl Future<Output = Result<Measurement, Error<IFACE::Error>>>;
}

impl<IFACE: AsyncIo, D: DelayNs> AsyncInterface<IFACE> for Opt3001<IFACE, D> {
    async fn init(&mut self) -> Result<(), Error<IFACE::Error>> {
        debug!("Reading OPT3001 manufacturer ID");
        let manufacturer = self.iface.read_register(REG_MANUFACTURER_ID).await?;
        if manufacturer != MANUFACTURER_ID {
            return Err(Error::InvalidManufacturerId(manufacturer));
        }

        debug!("Reading OPT3001 device ID");
        let device = self.iface.read_register(REG_DEVICE_ID).await?;
        if device != DEVICE_ID {
            return Err(Error::InvalidDeviceId(device));
        }

        self.reset().await?;
        debug!("OPT3001: Reset completed");
        self.initialized = true;
        Ok(())
    }

    async fn reset(&mut self) -> Result<(), Error<IFACE::Error>> {
        debug!("Writing OPT3001 configuration");
        self.iface
            .write_register(REG_CONFIGURATION, self.config.bits())
            .await?;
        debug!("OPT3001: Configuration written");
        Ok(())
    }

    async fn set_config(&mut self, config: Config) -> Result<(), Error<IFACE::Error>> {
        self.config = config;
        self.iface
            .write_register(REG_CONFIGURATION, config.bits())
            .await?;
        Ok(())
    }

    async fn status(&mut self) -> Result<Status, Error<IFACE::Error>> {
        Ok(Status::from(
            self.iface.read_register(REG_CONFIGURATION).await?,
        ))
    }

    async fn read(&mut self) -> Result<Measurement, Error<IFACE::Error>> {
        if !self.initialized {
            return Err(Error::NotInitialized);
        }

        if self.config.mode != Mode::Continuous {
            let config = self.config.with_mode(Mode::SingleShot);
            debug!("Starting OPT3001 single-shot conversion");
            self.iface
                .write_register(REG_CONFIGURATION, config.bits())
                .await?;
            self.delay
                .delay_ms(self.config.conversion_time.conversion_time_ms() + 10)
                .await;
        }

        let raw = self.iface.read_register(REG_RESULT).await?;
        let measurement = MeasurementRaw::try_from(raw).map_err(|_| Error::InvalidResult)?;
        #[cfg(feature = "float")]
        {
            Ok(measurement.into())
        }
        #[cfg(not(feature = "float"))]
        {
            Ok(measurement)
        }
    }
}
