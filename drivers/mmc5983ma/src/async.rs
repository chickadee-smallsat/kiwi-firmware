use core::{
    future::Future,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::{trace, MagMeasurement};
use embassy_sync::blocking_mutex::raw::RawMutex;

use crate::{
    config::Mmc5983Config,
    interface::{I2cInterface, Interface, SpiInterface},
    registers::{
        AnalogControl, DigitalControl, MeasurementTriggerControl, ProductId, Register,
        StatusRegister, MMC5983_DEVICE_ID,
    },
    ContinuousMeasurementFreq, MagMeasurementRaw, Mmc5983Async, Mmc5983Error, TempMeasurementRaw,
    MAX_LOOPS,
};
use embedded_hal_async::{delay::DelayNs, i2c, spi};

/// Asynchronous interface trait for the MMC5983MA sensor.
pub trait AsyncInterface: Interface {
    /// Error type for the interface operations.
    type Error;
    /// Writes data to the device.
    fn write(&mut self, data: &[u8]) -> impl Future<Output = Result<(), Self::Error>>;
    /// Reads data from the device.
    fn read(
        &mut self,
        address: u8,
        buffer: &mut [u8],
    ) -> impl Future<Output = Result<(), Self::Error>>;
}

impl<I2C, E> AsyncInterface for I2cInterface<I2C>
where
    I2C: i2c::I2c<Error = E>,
{
    type Error = E;

    async fn write(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        self.i2c.write(self.address, data).await
    }

    async fn read(&mut self, address: u8, buffer: &mut [u8]) -> Result<(), Self::Error> {
        self.i2c.write_read(self.address, &[address], buffer).await
    }
}

impl<SPI, E> AsyncInterface for SpiInterface<SPI>
where
    SPI: spi::SpiDevice<Error = E>,
{
    type Error = E;

    async fn write(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        self.spi.write(data).await
    }

    async fn read(&mut self, address: u8, buffer: &mut [u8]) -> Result<(), Self::Error> {
        self.spi
            .transaction(&mut [
                spi::Operation::Write(&[address]),
                spi::Operation::Read(buffer),
            ])
            .await
    }
}

pub(crate) trait AsyncRegister<IFACE>
where
    IFACE: AsyncInterface,
    Self: Register + Sized,
{
    async fn read_register(iface: &mut IFACE) -> Result<Self, IFACE::Error> {
        trace!("Reading register 0x{:02X}...", Self::ADDRESS);
        let mut data = [0u8];
        #[cfg(not(feature = "defmt-messages"))]
        iface.read(Self::ADDRESS, &mut data).await?;
        #[cfg(feature = "defmt-messages")]
        {
            let res = iface.read(Self::ADDRESS, &mut data).await;
            match res {
                Ok(()) => trace!("Data read: {=[u8]:02x}", data),
                Err(e) => {
                    trace!("Error reading register");
                    return Err(e);
                }
            }
        }
        Ok(Self::from_u8(data[0]))
    }

    async fn write_register(&self, iface: &mut IFACE) -> Result<(), IFACE::Error> {
        iface.write(&[Self::ADDRESS, self.to_u8()]).await?;
        Ok(())
    }
}

impl<IFACE> AsyncRegister<IFACE> for ProductId where IFACE: AsyncInterface {}
impl<IFACE> AsyncRegister<IFACE> for AnalogControl where IFACE: AsyncInterface {}
impl<IFACE> AsyncRegister<IFACE> for DigitalControl where IFACE: AsyncInterface {}
impl<IFACE> AsyncRegister<IFACE> for MeasurementTriggerControl where IFACE: AsyncInterface {}
impl<IFACE> AsyncRegister<IFACE> for StatusRegister where IFACE: AsyncInterface {}

impl MagMeasurementRaw {
    #[inline(always)]
    async fn read<IFACE: AsyncInterface>(iface: &mut IFACE) -> Result<Self, IFACE::Error> {
        let mut data = [0u8; 7];
        iface.read(0x0, &mut data).await?;
        Ok(Self::from(data))
    }
}

impl<IFACE, E, D, M: RawMutex> Mmc5983Async<IFACE, D, M>
where
    IFACE: AsyncInterface<Error = E>,
    D: DelayNs,
{
    /// Resets the device to its default state.
    pub async fn reset(&mut self) -> Result<(), Mmc5983Error<E>> {
        trace!("Resetting device...");
        let (mut analogctrl, measctrl, mut digitalctrl) = self.config.to_registers();
        {
            let (iface, delay) = &mut *self.core.lock().await;
            analogctrl.set_sw_rst(true);
            analogctrl.write_register(iface).await?;
            delay.delay_ms(10).await;
            analogctrl.set_sw_rst(false);
            analogctrl.write_register(iface).await?;
        }
        self.remove_offset().await?;
        {
            let (iface, _) = &mut *self.core.lock().await;
            digitalctrl.set_cm_freq(0);
            digitalctrl.set_cmm_en(false);
            digitalctrl.write_register(iface).await?;
            measctrl.write_register(iface).await?;
        }
        Ok(())
    }

    /// Measures the magnetic field and returns the raw measurement data.
    pub async fn measure(&self) -> Result<MagMeasurement, Mmc5983Error<E>> {
        let (iface, delay) = &mut *self.core.lock().await;
        Ok(
            MagMeasurement::from(get_mag(iface, delay, &self.config, &self.running).await?)
                - MagMeasurement::from(self.ofst),
        )
    }

    /// Measures the temperature and returns the raw temperature data.
    pub async fn temperature(&self) -> Result<TempMeasurementRaw, Mmc5983Error<E>> {
        let running = self.running.load(Ordering::Relaxed);
        if running {
            stop(self).await?;
        }
        let temp = {
            let (iface, delay) = &mut *self.core.lock().await;
            let (_, mut measctrl, _) = self.config.to_registers();
            measctrl.set_tm_t(true);
            measctrl.write_register(iface).await?;
            {
                let mut ctr = 0;
                loop {
                    let mut status = StatusRegister::read_register(iface).await?;

                    trace!(
                        "{}> Status [0x{:02X}]: 0x{:08b}",
                        ctr,
                        StatusRegister::ADDRESS,
                        status.to_u8()
                    );
                    if status.tmeas_done() {
                        let mut data = [0u8; 1];
                        iface.read(0x07, &mut data).await?;

                        trace!("Temp raw data: {=[u8]:02x}", data);
                        status.set_tmeas_done(true);
                        status.write_register(iface).await?;
                        break Some(TempMeasurementRaw(data[0]));
                    }
                    delay.delay_ms(1).await;
                    ctr += 1;
                    if ctr >= MAX_LOOPS {
                        trace!("Temperature measurement: timed out");
                        break None;
                    }
                }
            }
        };
        if running {
            start(self).await?;
        }
        temp.ok_or(Mmc5983Error::NotReady)
    }

    /// Measures and removes the soft iron offset from the sensor.
    pub async fn remove_offset(&mut self) -> Result<(), Mmc5983Error<E>> {
        trace!("Measuring and removing soft iron offset...");
        let running = self.running.load(Ordering::Relaxed);
        if running {
            stop(self).await?;
            self.running.store(false, Ordering::Relaxed);
        }
        if let Some((mag_set, mag_reset)) = {
            let (iface, delay) = &mut *self.core.lock().await;
            let (_, mut measctrl, _) = self.config.to_registers();
            measctrl.set_m_set(true);
            measctrl.write_register(iface).await?;
            delay.delay_ms(10).await;
            measctrl.set_tm_m(true);
            measctrl.write_register(iface).await?;
            let mag_set = {
                let mut ctr = 0;
                loop {
                    let mut status = StatusRegister::read_register(iface).await?;
                    if status.magmeas_done() {
                        let mag_set = get_mag(iface, delay, &self.config, &self.running).await?;
                        status.set_magmeas_done(true);
                        status.write_register(iface).await?;
                        break Some(mag_set);
                    }
                    delay.delay_ms(1).await;
                    ctr += 1;
                    if ctr >= MAX_LOOPS {
                        break None;
                    }
                }
            };
            measctrl.set_m_set(false);
            measctrl.set_m_reset(true);
            measctrl.write_register(iface).await?;
            delay.delay_ms(10).await;
            let mag_reset = {
                let mut ctr = 0;
                loop {
                    let mut status = StatusRegister::read_register(iface).await?;
                    if status.magmeas_done() {
                        let mag_reset = get_mag(iface, delay, &self.config, &self.running).await?;
                        status.set_magmeas_done(true);
                        status.write_register(iface).await?;
                        break Some(mag_reset);
                    }
                    delay.delay_ms(1).await;
                    ctr += 1;
                    if ctr >= MAX_LOOPS {
                        break None;
                    }
                }
            };
            measctrl.set_m_reset(false);
            measctrl.write_register(iface).await?;
            delay.delay_ms(10).await;
            if let (Some(mag_set), Some(mag_reset)) = (mag_set, mag_reset) {
                Some((mag_set, mag_reset))
            } else {
                trace!("Set/Reset measurements: timed out");
                None
            }
        } {
            self.update_offsets(mag_set, mag_reset);
        }
        if running {
            start(self).await?;
            self.running.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Initializes the device and applies the configuration.
    pub async fn init(&mut self) -> Result<(), Mmc5983Error<E>> {
        {
            let (iface, _) = &mut *self.core.lock().await;
            trace!("Stopping continuous measurement if enabled...");
            DigitalControl::new()
                .with_cm_freq(0x1)
                .with_cmm_en(true)
                .write_register(iface)
                .await?;
            DigitalControl::new()
                .with_cm_freq(0x1)
                .with_cmm_en(false)
                .write_register(iface)
                .await?;
            trace!("Reading device ID...");
            let dev_id = ProductId::read_register(iface).await?;

            trace!("Device ID: {:x}", dev_id.to_u8());
            if dev_id.to_u8() != MMC5983_DEVICE_ID {
                return Err(Mmc5983Error::InvalidDevice);
            }
        }
        self.reset().await
    }

    /// Starts continuous measurement mode and runs the provided async function.
    pub async fn start<'a: 'b, 'b, F, Fut>(&'a mut self, run: F) -> Result<(), Mmc5983Error<E>>
    where
        F: FnOnce(&'b Self) -> Fut,
        Fut: Future<Output = Result<(), Mmc5983Error<E>>>,
    {
        start(self).await?;
        self.running.store(true, Ordering::Relaxed);
        run(self).await?;
        stop(self).await?;
        self.running.store(false, Ordering::Relaxed);
        Ok(())
    }
}

async fn start<IFACE, D, E>(
    mmc: &Mmc5983Async<IFACE, D, impl RawMutex>,
) -> Result<(), Mmc5983Error<E>>
where
    IFACE: AsyncInterface<Error = E>,
{
    if mmc.running.load(Ordering::Relaxed) {
        return Ok(());
    }
    if mmc.config.frequency != ContinuousMeasurementFreq::Off {
        let (iface, _) = &mut *mmc.core.lock().await;
        let (_, _, mut digitalctrl) = mmc.config.to_registers();
        digitalctrl.set_cmm_en(true);
        digitalctrl.write_register(iface).await?;
        Ok(())
    } else {
        Err(Mmc5983Error::InvalidConfig)
    }
}

async fn stop<IFACE, D, E>(
    mmc: &Mmc5983Async<IFACE, D, impl RawMutex>,
) -> Result<(), Mmc5983Error<E>>
where
    IFACE: AsyncInterface<Error = E>,
{
    if !mmc.running.load(Ordering::Relaxed) {
        return Ok(());
    }
    if mmc.config.frequency != ContinuousMeasurementFreq::Off {
        let (iface, _) = &mut *mmc.core.lock().await;
        let (_, _, mut digitalctrl) = mmc.config.to_registers();
        digitalctrl.set_cmm_en(true);
        digitalctrl.write_register(iface).await?;
        digitalctrl.set_cmm_en(false);
        digitalctrl.set_cm_freq(0);
        digitalctrl.write_register(iface).await?;
        Ok(())
    } else {
        Err(Mmc5983Error::InvalidConfig)
    }
}

impl<I2C, D, M: RawMutex> Mmc5983Async<I2C, D, M> {
    pub(crate) fn update_offsets(
        &mut self,
        mag_set: MagMeasurementRaw,
        mag_reset: MagMeasurementRaw,
    ) {
        let x = (mag_set.x() + mag_reset.x()) / 2;
        let y = (mag_set.y() + mag_reset.y()) / 2;
        let z = (mag_set.z() + mag_reset.z()) / 2;
        let res = MagMeasurementRaw::new().with_x(x).with_y(y).with_z(z);

        trace!("Offsets: {}", res);
        self.ofst = Some(res);
    }
}

async fn get_mag<IFACE, D, E>(
    iface: &mut IFACE,
    delay: &mut D,
    config: &Mmc5983Config,
    running: &AtomicBool,
) -> Result<MagMeasurementRaw, Mmc5983Error<E>>
where
    IFACE: AsyncInterface<Error = E>,
    D: DelayNs,
{
    if !running.load(Ordering::Relaxed) {
        let (_, mut measctrl, _) = config.to_registers();
        measctrl.set_tm_m(true);
        measctrl.write_register(iface).await?;
        trace!(
            "Triggering measurement with delay of {} us",
            config.bandwidth.delay_us()
        );
        delay.delay_us(config.bandwidth.delay_us()).await;
    }
    let mut status = StatusRegister::read_register(iface).await?;
    trace!("Status [0x{:02X}]: {:?}", StatusRegister::ADDRESS, status);
    if status.magmeas_done() {
        status.set_magmeas_done(true);
        let mag = MagMeasurementRaw::read(iface).await?;
        status.write_register(iface).await?;
        Ok(mag)
    } else {
        Err(Mmc5983Error::NotReady)
    }
}
