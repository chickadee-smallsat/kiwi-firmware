use crate::{Measurement, debug};
use embedded_hal::{delay::DelayNs, i2c, spi};

use crate::{
    Error, Ms5607,
    details::{CMD_ADC_READ, CMD_CONV_D1, CMD_CONV_D2, CMD_PROM_READ, CMD_RESET, Calibration},
    interface::{I2cInterface, Interface, SpiInterface},
};

/// Synchronous I/O interface for the MS5607 device
pub trait SyncIo: Interface {
    /// The error type for asynchronous operations
    type Error;

    /// Write a single byte to the device (used for sending commands)
    fn write(&mut self, value: u8) -> Result<(), Self::Error>;

    /// Read bytes from the device (used for reading calibration data and raw measurements)
    fn read(&mut self, address: u8, buffer: &mut [u8]) -> Result<(), Self::Error>;
}

impl<I2C, E> SyncIo for I2cInterface<I2C>
where
    I2C: i2c::I2c<Error = E>,
{
    type Error = I2C::Error;

    fn write(&mut self, value: u8) -> Result<(), Self::Error> {
        let i2c_addr = self.address;
        let i2c = &mut self.i2c;
        i2c.write(i2c_addr, &[value])
    }

    fn read(&mut self, address: u8, buffer: &mut [u8]) -> Result<(), Self::Error> {
        use i2c::Operation::{Read, Write};
        let i2c_addr = self.address;
        let i2c = &mut self.i2c;
        i2c.transaction(i2c_addr, &mut [Write(&[address]), Read(buffer)])
    }
}

impl<SPI, E> SyncIo for SpiInterface<SPI>
where
    SPI: spi::SpiDevice<Error = E>,
{
    type Error = SPI::Error;

    fn write(&mut self, value: u8) -> Result<(), Self::Error> {
        let spi = &mut self.spi;
        spi.write(&[value])
    }

    fn read(&mut self, address: u8, buffer: &mut [u8]) -> Result<(), Self::Error> {
        use spi::Operation::{Read, Write};
        let spi = &mut self.spi;
        spi.transaction(&mut [Write(&[address]), Read(buffer)])
    }
}
pub trait Read<IFACE: SyncIo> {
    fn read(iface: &mut IFACE) -> Result<Self, Error<IFACE::Error>>
    where
        Self: Sized;
}

impl<IFACE: SyncIo> Read<IFACE> for Calibration {
    fn read(iface: &mut IFACE) -> Result<Self, Error<IFACE::Error>> {
        let mut buffer = [0u8; 16];

        for ofst in (0..0xf).step_by(2) {
            iface.read(
                CMD_PROM_READ + ofst,
                &mut buffer[ofst as usize..(ofst + 2) as usize],
            )?;
        }
        Self::try_from(buffer).map_err(|_| Error::Crc)
    }
}

/// Synchronous interface for the MS5607 device
pub trait SyncInterface<IFACE: SyncIo> {
    /// Initialize the device by resetting it and reading the factory calibration data
    fn init(&mut self) -> Result<(), Error<IFACE::Error>>;
    /// Reset the device (resets the internal state and reads the factory calibration data)
    fn reset(&mut self) -> Result<(), Error<IFACE::Error>>;
    /// Read the compensated pressure and temperature values from the device
    fn read(&mut self) -> Result<Measurement, Error<IFACE::Error>>;
}

impl<IFACE: SyncIo, D: DelayNs> SyncInterface<IFACE> for Ms5607<IFACE, D> {
    fn init(&mut self) -> Result<(), Error<IFACE::Error>> {
        self.reset()
    }

    fn reset(&mut self) -> Result<(), Error<IFACE::Error>> {
        debug!("Resetting MS5607");
        self.iface.write(CMD_RESET)?;
        debug!("Waiting 3 ms for reset to complete");
        self.delay.delay_us(3000);
        debug!("Reset complete, reading factory calibration data");
        self.calibration = Some(Calibration::read(&mut self.iface)?);
        Ok(())
    }

    fn read(&mut self) -> Result<Measurement, Error<IFACE::Error>> {
        let mut buf = [0u8; 4];
        let conversion_time_us = self.osr.conversion_time_us();
        // Read raw pressure
        debug!("Starting pressure conversion");
        self.iface.write(CMD_CONV_D1 + self.osr as u8)?;
        debug!(
            "Waiting {} us for pressure conversion to complete",
            conversion_time_us
        );
        self.delay.delay_us(conversion_time_us);
        debug!("Reading raw pressure");
        self.iface.read(CMD_ADC_READ, &mut buf[1..])?;
        let raw_pressure = u32::from_be_bytes(buf);
        debug!("Raw pressure: {}", raw_pressure);
        // Read raw temperature
        debug!("Starting temperature conversion");
        self.iface.write(CMD_CONV_D2 + self.osr as u8)?;
        debug!(
            "Waiting {} us for temperature conversion to complete",
            conversion_time_us
        );
        self.delay.delay_us(conversion_time_us);
        debug!("Reading raw temperature");
        self.iface.read(CMD_ADC_READ, &mut buf[1..])?;
        let raw_temperature = u32::from_be_bytes(buf);
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
