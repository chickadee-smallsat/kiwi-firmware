use core::sync::atomic::Ordering;

use crate::{debug, error, info, trace, warn};
use embedded_hal::{delay::DelayNs, i2c, spi};

use crate::{
    conversion::CalibrationCoefficients,
    interface::Interface,
    registers::{
        Command, DeviceId, ErrorReg, EventReg, IrqControl, IrqStatus, OversamplingReg, PowerCtrl,
        Register, StatusReg, NVM_PAR_T1_0, PRESSURE_DATA_ADDR,
    },
    Bmp390Error, Bmp390Sync, I2cInterface, IIRFilterConfig, Measurement, OutputDataRate,
    SensorMode, SpiInterface, MAX_LOOPS,
};

/// Synchronous interface for the BMP390 sensor.
pub trait SyncInterface: Interface {
    /// The error type for the interface operations.
    type Error;
    /// Writes data to the device.
    fn write(&mut self, data: &[u8]) -> Result<(), Self::Error>;
    /// Reads data from the device starting at the given address into the provided buffer.
    fn read(&mut self, address: u8, buffer: &mut [u8]) -> Result<(), Self::Error>;
}

impl<I2C, E> SyncInterface for I2cInterface<I2C>
where
    I2C: i2c::I2c<Error = E>,
{
    type Error = E;

    fn write(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        self.i2c.write(self.address, data)
    }

    fn read(&mut self, address: u8, buffer: &mut [u8]) -> Result<(), Self::Error> {
        self.i2c.transaction(
            self.address,
            &mut [
                i2c::Operation::Write(&[address]),
                i2c::Operation::Read(buffer),
            ],
        )?;
        trace!("I2C Read from {:#x}: {=[u8]:#x}", address, buffer);
        Ok(())
    }
}

impl<SPI, E> SyncInterface for SpiInterface<SPI>
where
    SPI: spi::SpiDevice<Error = E>,
{
    type Error = E;

    fn write(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        {
            trace!("SPI Write: {=[u8]:#x}", data);
        }
        self.spi.write(data)
    }

    fn read(&mut self, address: u8, buffer: &mut [u8]) -> Result<(), Self::Error> {
        let mut dummy = [0u8; 1];
        self.spi.transaction(&mut [
            spi::Operation::Write(&[address]),
            spi::Operation::Read(&mut dummy),
            spi::Operation::Read(buffer),
        ])?;
        trace!("SPI Read from {:#x}: {=[u8]:#x}", address, buffer);
        Ok(())
    }
}

pub(crate) trait SyncRegister<IFACE>
where
    IFACE: SyncInterface,
    Self: Register + Sized,
{
    fn read_register(iface: &mut IFACE) -> Result<Self, IFACE::Error> {
        let mut data = [0u8; 1];
        iface.read(Self::ADDRESS, &mut data)?;
        Ok(Self::from_u8(data[0]))
    }

    fn write_register(&self, iface: &mut IFACE) -> Result<(), IFACE::Error> {
        let mut buf = [0; 2];
        buf[0] = Self::ADDRESS;
        buf[1] = self.to_u8();
        iface.write(&buf)?;
        Ok(())
    }
}

macro_rules! impl_async_register {
    ($($reg:ty),+) => {
        $(
            impl<IFACE> SyncRegister<IFACE> for $reg
            where
                IFACE: SyncInterface,
            {}
        )+
    };
}

impl_async_register! {DeviceId, ErrorReg, StatusReg, EventReg, IrqStatus, IrqControl, PowerCtrl, OversamplingReg, OutputDataRate, IIRFilterConfig, Command}

impl<IFACE, E, D> Bmp390Sync<IFACE, D>
where
    IFACE: SyncInterface<Error = E>,
    D: DelayNs,
{
    /// Initialize the BMP390 device, apply the configuration, and read calibration coefficients.
    pub fn init(&mut self) -> Result<CalibrationCoefficients, Bmp390Error<E>> {
        let mut coeffs_buf = [0u8; 21];
        {
            self.delay.delay_ms(2);
            // Check device ID
            let device_id = DeviceId::read_register(&mut self.iface)?;
            if !device_id.validate() {
                return Err(Bmp390Error::InvalidDevice);
            }
            // Read calibration coefficients
            self.iface.read(NVM_PAR_T1_0, &mut coeffs_buf)?;
        }
        self.reset()?;
        Ok(CalibrationCoefficients::from_registers(&coeffs_buf))
    }

    /// Reset the BMP390 device and apply the configuration.
    pub fn reset(&mut self) -> Result<(), Bmp390Error<E>> {
        // Reset device
        Command::SoftReset.write_register(&mut self.iface)?;
        self.delay.delay_ms(20);
        debug!("BMP390 reset complete");
        debug!(
            "IRQ Control [{=u8:#x}]: {=u8:#b}",
            IrqControl::ADDRESS,
            self.config.irq_control.to_u8()
        );
        debug!(
            "Filter Config [{=u8:#x}]: {=u8:#b}",
            IIRFilterConfig::ADDRESS,
            self.config.filtercfg.to_u8()
        );
        debug!(
            "ODR [{=u8:#x}]: {=u8:#b}",
            OutputDataRate::ADDRESS,
            self.config.odr.to_u8()
        );
        debug!(
            "Oversampling [{=u8:#x}]: {=u8:#b}",
            OversamplingReg::ADDRESS,
            self.config.oversamp_config().to_u8()
        );
        PowerCtrl::new()
            .with_mode(self.config.sensor_mode)
            .with_press_en(true)
            .with_temp_en(true)
            .write_register(&mut self.iface)?;
        let err = ErrorReg::read_register(&mut self.iface)?;
        if err.fatal() {
            {
                error!("Fatal error during BMP390 reset: Setting normal mode");
            }
            return Err(Bmp390Error::FatalError);
        } else if err.configuration() {
            {
                error!("Configuration error during BMP390 reset: Setting normal mode");
            }
            return Err(Bmp390Error::InvalidConfiguration);
        } else if err.command() {
            {
                error!("Command error during BMP390 reset: Setting normal mode");
            }
            return Err(Bmp390Error::InvalidCommand);
        }
        self.config.irq_control.write_register(&mut self.iface)?;
        let err = ErrorReg::read_register(&mut self.iface)?;
        if err.fatal() {
            {
                error!("Fatal error during BMP390 reset: Writing IRQ control config");
            }
            return Err(Bmp390Error::FatalError);
        } else if err.configuration() {
            {
                error!("Configuration error during BMP390 reset: Writing IRQ control config");
            }
            return Err(Bmp390Error::InvalidConfiguration);
        } else if err.command() {
            {
                error!("Command error during BMP390 reset: Writing IRQ control config");
            }
            return Err(Bmp390Error::InvalidCommand);
        }
        self.config.filtercfg.write_register(&mut self.iface)?;
        let err = ErrorReg::read_register(&mut self.iface)?;
        if err.fatal() {
            {
                error!("Fatal error during BMP390 reset: Writing filter config");
            }
            return Err(Bmp390Error::FatalError);
        } else if err.configuration() {
            {
                error!("Configuration error during BMP390 reset: Writing filter config");
            }
            return Err(Bmp390Error::InvalidConfiguration);
        } else if err.command() {
            {
                error!("Command error during BMP390 reset: Writing filter config");
            }
            return Err(Bmp390Error::InvalidCommand);
        }
        self.config.odr.write_register(&mut self.iface)?;
        let err = ErrorReg::read_register(&mut self.iface)?;
        if err.fatal() {
            {
                error!("Fatal error during BMP390 reset: Writing ODR config");
            }
            return Err(Bmp390Error::FatalError);
        } else if err.configuration() {
            {
                error!("Configuration error during BMP390 reset: Writing ODR config");
            }
            return Err(Bmp390Error::InvalidConfiguration);
        } else if err.command() {
            {
                error!("Command error during BMP390 reset: Writing ODR config");
            }
            return Err(Bmp390Error::InvalidCommand);
        }
        self.config
            .oversamp_config()
            .write_register(&mut self.iface)?;
        let error = ErrorReg::read_register(&mut self.iface)?;
        if error.fatal() {
            {
                error!("Fatal error during BMP390 reset: Writing oversampling config");
            }
            Err(Bmp390Error::FatalError)
        } else if error.configuration() {
            {
                error!("Configuration error during BMP390 reset: Writing oversampling config");
            }
            Err(Bmp390Error::InvalidConfiguration)
        } else if error.command() {
            {
                error!("Command error during BMP390 reset: Writing oversampling config");
            }
            Err(Bmp390Error::InvalidCommand)
        } else {
            Ok(())
        }
    }

    /// Start continuous measurement mode.
    pub fn start(&mut self) -> Result<(), Bmp390Error<E>> {
        start(self)
    }

    /// Stop continuous measurement mode.
    pub fn stop(&mut self) -> Result<(), Bmp390Error<E>> {
        stop(self)
    }

    /// Reads a measurement from the device.
    pub fn measure(&mut self) -> Result<Measurement, Bmp390Error<E>> {
        if !self.running.load(Ordering::Relaxed) {
            return Err(Bmp390Error::NotReady);
        }
        let status = if !self.config.irq_control.drdy() {
            let mut ctr = MAX_LOOPS;
            loop {
                let status = StatusReg::read_register(&mut self.iface)?;
                if status.press_drdy() || status.temp_drdy() {
                    break status;
                }
                self.delay.delay_us(self.config.min_delay_us);
                ctr -= 1;
                if ctr == 0 {
                    return Err(Bmp390Error::NoDataAvailable);
                }
            }
        } else {
            StatusReg::read_register(&mut self.iface)?
        };

        trace!("Status: {=u8:#x}", status.to_u8());
        let read = status.temp_drdy() || status.press_drdy();
        if !read {
            return Err(Bmp390Error::NoDataAvailable);
        }
        let mut buf = [0u8; 6];
        self.iface.read(PRESSURE_DATA_ADDR, &mut buf)?;
        let pressure_data = if status.press_drdy() {
            let meas = u32::from_le_bytes([buf[0], buf[1], buf[2], 0]);
            Some(meas)
        } else {
            None
        };
        let temp_data = if status.temp_drdy() {
            let meas = u32::from_le_bytes([buf[3], buf[4], buf[5], 0]);
            Some(meas)
        } else {
            None
        };
        Ok(Measurement {
            pressure: pressure_data,
            temperature: temp_data,
        })
    }
}

fn start<IFACE, D>(sensor: &mut Bmp390Sync<IFACE, D>) -> Result<(), Bmp390Error<IFACE::Error>>
where
    IFACE: SyncInterface,
    D: DelayNs,
{
    if sensor.running.load(Ordering::Relaxed) {
        return Ok(());
    }
    sensor
        .config
        .power_ctrl()
        .write_register(&mut sensor.iface)?;
    info!(
        "Power Control [{=u8:#x}]: {=u8:#b}",
        PowerCtrl::ADDRESS,
        sensor.config.power_ctrl().to_u8()
    );
    let error = ErrorReg::read_register(&mut sensor.iface)?;

    {
        if error.to_u8() != 0 {
            warn!(
                "Error Register [{=u8:#x}]: {=u8:#b}",
                ErrorReg::ADDRESS,
                error.to_u8()
            );
        } else {
            info!(
                "Error Register [{=u8:#x}]: {=u8:#b}",
                ErrorReg::ADDRESS,
                error.to_u8()
            );
        }
    }
    if error.fatal() {
        return Err(Bmp390Error::FatalError);
    } else if error.configuration() {
        return Err(Bmp390Error::InvalidConfiguration);
    } else if error.command() {
        return Err(Bmp390Error::InvalidCommand);
    }
    sensor.running.store(true, Ordering::Relaxed);
    Ok(())
}

fn stop<IFACE, D>(sensor: &mut Bmp390Sync<IFACE, D>) -> Result<(), Bmp390Error<IFACE::Error>>
where
    IFACE: SyncInterface,
    D: DelayNs,
{
    if !sensor.running.load(Ordering::Relaxed) {
        return Ok(());
    }
    // Disable sensors
    PowerCtrl::new()
        .with_mode(SensorMode::Sleep)
        .with_press_en(false)
        .with_temp_en(false)
        .write_register(&mut sensor.iface)?;
    let error = ErrorReg::read_register(&mut sensor.iface)?;
    if error.fatal() {
        return Err(Bmp390Error::FatalError);
    } else if error.configuration() {
        return Err(Bmp390Error::InvalidConfiguration);
    } else if error.command() {
        return Err(Bmp390Error::InvalidCommand);
    }
    sensor.running.store(false, Ordering::Relaxed);
    Ok(())
}
