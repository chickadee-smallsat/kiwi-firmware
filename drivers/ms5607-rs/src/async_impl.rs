use crate::{Measurement, debug};
use embedded_hal_async::{delay::DelayNs, i2c, spi};

use crate::{
    Error, Ms5607,
    details::{CMD_ADC_READ, CMD_CONV_D1, CMD_CONV_D2, CMD_PROM_READ, CMD_RESET, Calibration},
    interface::{I2cInterface, Interface, SpiInterface},
};
use core::future::Future;

/// Asynchronous I/O interface for the MS5607 device
pub trait AsyncIo: Interface {
    /// The error type for asynchronous operations
    type Error;

    /// Write a single byte to the device (used for sending commands)
    fn write(&mut self, value: u8) -> impl Future<Output = Result<(), Self::Error>>;

    /// Read bytes from the device (used for reading calibration data and raw measurements)
    fn read(
        &mut self,
        address: u8,
        buffer: &mut [u8],
    ) -> impl Future<Output = Result<(), Self::Error>>;
}

impl<I2C, E> AsyncIo for I2cInterface<I2C>
where
    I2C: i2c::I2c<Error = E>,
{
    type Error = I2C::Error;

    fn write(&mut self, value: u8) -> impl Future<Output = Result<(), Self::Error>> {
        let i2c_addr = self.address;
        let i2c = &mut self.i2c;
        async move { i2c.write(i2c_addr, &[value]).await }
    }

    fn read(
        &mut self,
        address: u8,
        buffer: &mut [u8],
    ) -> impl Future<Output = Result<(), Self::Error>> {
        use i2c::Operation::{Read, Write};
        let i2c_addr = self.address;
        let i2c = &mut self.i2c;
        async move {
            i2c.transaction(i2c_addr, &mut [Write(&[address]), Read(buffer)])
                .await
        }
    }
}

impl<SPI, E> AsyncIo for SpiInterface<SPI>
where
    SPI: spi::SpiDevice<Error = E>,
{
    type Error = SPI::Error;

    fn write(&mut self, value: u8) -> impl Future<Output = Result<(), Self::Error>> {
        let spi = &mut self.spi;
        async move { spi.write(&[value]).await }
    }

    fn read(
        &mut self,
        address: u8,
        buffer: &mut [u8],
    ) -> impl Future<Output = Result<(), Self::Error>> {
        use spi::Operation::{Read, Write};
        let spi = &mut self.spi;
        async move {
            spi.transaction(&mut [Write(&[address]), Read(buffer)])
                .await
        }
    }
}

pub trait Read<IFACE: AsyncIo> {
    fn read(iface: &mut IFACE) -> impl Future<Output = Result<Self, Error<IFACE::Error>>>
    where
        Self: Sized;
}

impl<IFACE: AsyncIo> Read<IFACE> for Calibration {
    async fn read(iface: &mut IFACE) -> Result<Self, Error<IFACE::Error>> {
        let mut buffer = [0u8; 16];

        for ofst in (0..0xf).step_by(2) {
            iface
                .read(
                    CMD_PROM_READ + ofst,
                    &mut buffer[ofst as usize..(ofst + 2) as usize],
                )
                .await?;
        }
        Self::try_from(buffer).map_err(|_| Error::Crc)
    }
}

/// Asynchronous interface for the MS5607 device
pub trait AsyncInterface<IFACE: AsyncIo> {
    /// Initialize the device by resetting it and reading the factory calibration data
    fn init(&mut self) -> impl Future<Output = Result<(), Error<IFACE::Error>>>;
    /// Reset the device (resets the internal state and reads the factory calibration data)
    fn reset(&mut self) -> impl Future<Output = Result<(), Error<IFACE::Error>>>;
    /// Read the compensated pressure and temperature values from the device
    fn read(&mut self) -> impl Future<Output = Result<Measurement, Error<IFACE::Error>>>;
}

impl<IFACE: AsyncIo, D: DelayNs> AsyncInterface<IFACE> for Ms5607<IFACE, D> {
    async fn init(&mut self) -> Result<(), Error<IFACE::Error>> {
        self.reset().await?;
        Ok(())
    }

    async fn reset(&mut self) -> Result<(), Error<IFACE::Error>> {
        debug!("Resetting MS5607");
        self.iface.write(CMD_RESET).await?;
        debug!("Waiting 3 ms for reset to complete");
        self.delay.delay_ms(3).await;
        debug!("Reset complete, reading factory calibration data");
        self.calibration = Some(Calibration::read(&mut self.iface).await?);
        Ok(())
    }

    async fn read(&mut self) -> Result<Measurement, Error<IFACE::Error>> {
        let mut buf = [0u8; 4];
        let conversion_time_us = self.osr.conversion_time_us();
        // Read raw pressure
        debug!("Starting pressure conversion");
        self.iface.write(CMD_CONV_D1 + self.osr as u8).await?;
        debug!(
            "Waiting {} us for pressure conversion to complete",
            conversion_time_us
        );
        self.delay.delay_us(conversion_time_us).await;
        debug!("Reading raw pressure");
        self.iface.read(CMD_ADC_READ, &mut buf[1..]).await?;
        let raw_pressure = u32::from_be_bytes(buf);
        debug!("Raw pressure: {}", raw_pressure);
        // Read raw temperature
        debug!("Starting temperature conversion");
        self.iface.write(CMD_CONV_D2 + self.osr as u8).await?;
        debug!(
            "Waiting {} us for temperature conversion to complete",
            conversion_time_us
        );
        self.delay.delay_us(conversion_time_us).await;
        debug!("Reading raw temperature");
        self.iface.read(CMD_ADC_READ, &mut buf[1..]).await?;
        let raw_temperature = (u32::from_be_bytes(buf) << 8) >> 8; // The raw temperature is only 24 bits, so we need to sign-extend it
        debug!("Raw temperature: {}", raw_temperature);
        // Compensate readings using factory calibration data
        let res = self
            .calibration
            .as_ref()
            .ok_or(Error::NotInitialized)?
            .compensate(raw_pressure, raw_temperature);
        #[cfg(feature = "float")]
        {
            Ok(res.into())
        }
        #[cfg(not(feature = "float"))]
        {
            Ok(res)
        }
    }
}
