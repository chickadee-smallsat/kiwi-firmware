use crate::{config::Mmc5983Config, debug, MagMeasurement};
use core::sync::atomic::{AtomicBool, Ordering};
use embedded_hal::{delay::DelayNs, i2c::I2c, spi};

use crate::{
    interface::{I2cInterface, Interface, SpiInterface},
    registers::{
        AnalogControl, DigitalControl, MeasurementTriggerControl, ProductId, Register,
        StatusRegister, XYZOut2, MMC5983_DEVICE_ID,
    },
    ContinuousMeasurementFreq, MagMeasurementRaw, Mmc5983Error, Mmc5983Sync, TempMeasurementRaw,
    MAX_LOOPS,
};

/// Synchronous interface for the MMC5983MA sensor.
pub trait SyncInterface: Interface {
    /// Error type for the interface.
    type Error;
    /// Writes data to the device.
    fn write(&mut self, data: &[u8]) -> Result<(), Self::Error>;
    /// Reads data from the device.
    fn read(&mut self, address: u8, buffer: &mut [u8]) -> Result<(), Self::Error>;
}

impl<I2C, E> SyncInterface for I2cInterface<I2C>
where
    I2C: I2c<Error = E>,
{
    type Error = E;

    fn write(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        self.i2c.write(self.address, data)
    }

    fn read(&mut self, address: u8, buffer: &mut [u8]) -> Result<(), Self::Error> {
        self.i2c.write_read(self.address, &[address], buffer)
    }
}

impl<SPI, E> SyncInterface for SpiInterface<SPI>
where
    SPI: spi::SpiDevice<Error = E>,
{
    type Error = E;

    fn write(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        self.spi.write(data)
    }

    fn read(&mut self, address: u8, buffer: &mut [u8]) -> Result<(), Self::Error> {
        self.spi.transaction(&mut [
            spi::Operation::Write(&[address]),
            spi::Operation::Read(buffer),
        ])
    }
}

pub(crate) trait SyncRegister<I>
where
    I: SyncInterface,
    Self: Register + Sized,
{
    fn read_register(iface: &mut I) -> Result<Self, I::Error> {
        let mut data = [0u8];
        iface.read(Self::ADDRESS, &mut data)?;
        Ok(Self::from_u8(data[0]))
    }

    fn write_register(&self, iface: &mut I) -> Result<(), I::Error> {
        iface.write(&[Self::ADDRESS, self.to_u8()])?;
        Ok(())
    }
}

impl<I> SyncRegister<I> for ProductId where I: SyncInterface {}
impl<I> SyncRegister<I> for AnalogControl where I: SyncInterface {}
impl<I> SyncRegister<I> for DigitalControl where I: SyncInterface {}
impl<I> SyncRegister<I> for MeasurementTriggerControl where I: SyncInterface {}
impl<I> SyncRegister<I> for StatusRegister where I: SyncInterface {}
impl<I> SyncRegister<I> for XYZOut2 where I: SyncInterface {}

impl<IFACE, E, D> Mmc5983Sync<IFACE, D>
where
    IFACE: SyncInterface<Error = E>,
    D: DelayNs,
{
    /// Reset the device and apply the configuration.
    pub fn reset(&mut self) -> Result<(), Mmc5983Error<E>> {
        let (mut analogctrl, measctrl, mut digitalctrl) = self.config.to_registers();
        {
            analogctrl.set_sw_rst(true);
            analogctrl.write_register(&mut self.iface)?;
            self.delay.delay_ms(10);
            analogctrl.set_sw_rst(false);
            analogctrl.write_register(&mut self.iface)?;
        }
        self.remove_offset()?;
        {
            digitalctrl.set_cm_freq(0);
            digitalctrl.set_cmm_en(false);
            digitalctrl.write_register(&mut self.iface)?;
            measctrl.write_register(&mut self.iface)?;
        }
        Ok(())
    }

    /// Get a raw magnetometer measurement from the device.
    ///
    /// If the device is not in continuous measurement mode, a single
    /// measurement will be triggered.
    pub fn measure(&mut self) -> Result<MagMeasurement, Mmc5983Error<E>> {
        Ok(MagMeasurement::from(get_mag(
            &mut self.iface,
            &mut self.delay,
            &self.config,
            &self.running,
        )?) - MagMeasurement::from(self.ofst))
    }

    /// Get a raw temperature measurement from the device.
    ///
    /// This function will temporarily stop continuous measurement mode if it is active.
    pub fn temperature(&mut self) -> Result<TempMeasurementRaw, Mmc5983Error<E>> {
        let running = self.running.load(Ordering::Relaxed);
        if running {
            stop(self)?;
        }
        let temp = {
            let (_, mut measctrl, _) = self.config.to_registers();
            measctrl.set_tm_t(true);
            measctrl.write_register(&mut self.iface)?;
            {
                let mut ctr = 0;
                loop {
                    let mut status = StatusRegister::read_register(&mut self.iface)?;

                    debug!(
                        "{}> Status [0x{:02X}]: 0x{:08b}",
                        ctr,
                        StatusRegister::ADDRESS,
                        status.to_u8()
                    );
                    if status.tmeas_done() {
                        let mut data = [0u8; 1];
                        self.iface.read(0x07, &mut data)?;

                        debug!("Temp raw data: {=[u8]:02x}", data);
                        status.set_tmeas_done(true);
                        status.write_register(&mut self.iface)?;
                        break Some(TempMeasurementRaw(data[0]));
                    }
                    self.delay.delay_ms(1);
                    ctr += 1;
                    if ctr >= MAX_LOOPS {
                        debug!("Temperature measurement: timed out");
                        break None;
                    }
                }
            }
        };
        if running {
            start(self)?;
        }
        temp.ok_or(Mmc5983Error::NotReady)
    }

    /// Remove the soft iron offset using the SET/RESET method.
    pub fn remove_offset(&mut self) -> Result<(), Mmc5983Error<E>> {
        let running = self.running.load(Ordering::Relaxed);
        if running {
            self.stop()?;
        }
        if let Some((mag_set, mag_reset)) = {
            let (_, mut measctrl, _) = self.config.to_registers();
            measctrl.set_m_set(true);
            measctrl.write_register(&mut self.iface)?;
            self.delay.delay_ms(10);
            measctrl.set_tm_m(true);
            measctrl.write_register(&mut self.iface)?;
            let mag_set = {
                let mut ctr = 0;
                loop {
                    let mut status = StatusRegister::read_register(&mut self.iface)?;
                    if status.magmeas_done() {
                        let mag_set = get_mag(
                            &mut self.iface,
                            &mut self.delay,
                            &self.config,
                            &self.running,
                        )?;
                        status.set_magmeas_done(true);
                        status.write_register(&mut self.iface)?;
                        break Some(mag_set);
                    }
                    self.delay.delay_ms(1);
                    ctr += 1;
                    if ctr >= MAX_LOOPS {
                        break None;
                    }
                }
            };
            measctrl.set_m_set(false);
            measctrl.set_m_reset(true);
            measctrl.write_register(&mut self.iface)?;
            self.delay.delay_ms(10);
            let mag_reset = {
                let mut ctr = 0;
                loop {
                    let mut status = StatusRegister::read_register(&mut self.iface)?;
                    if status.magmeas_done() {
                        let mag_reset = get_mag(
                            &mut self.iface,
                            &mut self.delay,
                            &self.config,
                            &self.running,
                        )?;
                        status.set_magmeas_done(true);
                        status.write_register(&mut self.iface)?;
                        break Some(mag_reset);
                    }
                    self.delay.delay_ms(1);
                    ctr += 1;
                    if ctr >= MAX_LOOPS {
                        break None;
                    }
                }
            };
            measctrl.set_m_reset(false);
            measctrl.write_register(&mut self.iface)?;
            self.delay.delay_ms(10);
            if let (Some(mag_set), Some(mag_reset)) = (mag_set, mag_reset) {
                Some((mag_set, mag_reset))
            } else {
                debug!("Set/Reset measurements: timed out");
                None
            }
        } {
            self.update_offsets(mag_set, mag_reset);
        }
        if running {
            start(self)?;
            self.running.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Stop continuous measurement mode.
    pub fn stop(&mut self) -> Result<(), Mmc5983Error<E>> {
        stop(self)?;
        self.running.store(false, Ordering::Relaxed);
        Ok(())
    }

    /// Initialize the device and apply the configuration.
    pub fn init(&mut self) -> Result<(), Mmc5983Error<E>> {
        {
            debug!("Reading device ID...");
            let dev_id = ProductId::read_register(&mut self.iface)?;

            debug!("Device ID: {:x}", dev_id.to_u8());
            if dev_id.to_u8() != MMC5983_DEVICE_ID {
                return Err(Mmc5983Error::InvalidDevice);
            }
        }
        self.reset()
    }

    /// Start continuous measurement mode.
    pub fn start(&mut self) -> Result<(), Mmc5983Error<E>> {
        start(self)?;
        self.running.store(true, Ordering::Relaxed);
        Ok(())
    }
}

fn start<IFACE, D, E>(mmc: &mut Mmc5983Sync<IFACE, D>) -> Result<(), Mmc5983Error<E>>
where
    IFACE: SyncInterface<Error = E>,
{
    if mmc.running.load(Ordering::Relaxed) {
        return Ok(());
    }
    if mmc.config.frequency != ContinuousMeasurementFreq::Off {
        let (_, _, mut digitalctrl) = mmc.config.to_registers();
        digitalctrl.set_cmm_en(true);
        digitalctrl.write_register(&mut mmc.iface)?;
        Ok(())
    } else {
        Err(Mmc5983Error::InvalidConfig)
    }
}

fn stop<IFACE, D, E>(mmc: &mut Mmc5983Sync<IFACE, D>) -> Result<(), Mmc5983Error<E>>
where
    IFACE: SyncInterface<Error = E>,
{
    if !mmc.running.load(Ordering::Relaxed) {
        return Ok(());
    }
    if mmc.config.frequency != ContinuousMeasurementFreq::Off {
        let (_, _, mut digitalctrl) = mmc.config.to_registers();
        digitalctrl.set_cmm_en(true);
        digitalctrl.write_register(&mut mmc.iface)?;
        digitalctrl.set_cmm_en(false);
        digitalctrl.set_cm_freq(0);
        digitalctrl.write_register(&mut mmc.iface)?;
        Ok(())
    } else {
        Err(Mmc5983Error::InvalidConfig)
    }
}

impl<I2C, D> Mmc5983Sync<I2C, D> {
    pub(crate) fn update_offsets(
        &mut self,
        mag_set: MagMeasurementRaw,
        mag_reset: MagMeasurementRaw,
    ) {
        let x = (mag_set.x() + mag_reset.x()) / 2;
        let y = (mag_set.y() + mag_reset.y()) / 2;
        let z = (mag_set.z() + mag_reset.z()) / 2;
        let res = MagMeasurementRaw::new().with_x(x).with_y(y).with_z(z);
        debug!("Offsets: {}", res);
        self.ofst = Some(res);
    }
}

fn get_mag<IFACE, D, E>(
    iface: &mut IFACE,
    delay: &mut D,
    config: &Mmc5983Config,
    running: &AtomicBool,
) -> Result<MagMeasurementRaw, Mmc5983Error<E>>
where
    IFACE: SyncInterface<Error = E>,
    D: DelayNs,
{
    if !running.load(Ordering::Relaxed) {
        let (_, mut measctrl, _) = config.to_registers();
        measctrl.set_tm_m(true);
        measctrl.write_register(iface)?;
        delay.delay_us(config.bandwidth.delay_us());
    }
    let mut status = StatusRegister::read_register(iface)?;
    if status.magmeas_done() {
        status.set_magmeas_done(true);
        let mut data = [0u8; 7];
        iface.read(0x0, &mut data)?;
        status.write_register(iface)?;
        let mag = MagMeasurementRaw::from(data);
        Ok(mag)
    } else {
        Err(Mmc5983Error::NotReady)
    }
}
