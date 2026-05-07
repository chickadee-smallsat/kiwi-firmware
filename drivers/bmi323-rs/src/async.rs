use crate::{debug, info, trace, warn};
use core::{future::Future, sync::atomic::Ordering};
use embassy_sync::blocking_mutex::raw::RawMutex;

use crate::{
    config::AccGyroEnabled,
    interface::{I2cInterface, Interface, SpiInterface},
    registers::{
        AccelConfig, Command, DeviceId, ErrorReg, FeatEngAddr, FeatEngConfig, FeatEngIo0,
        FeatEngIoStat, FeatureDataStatus, FeatureEngineControl, FeatureEngineStatus,
        FeatureInterruptMap, FeatureIo1Error, FifoConfig, FifoCtrl, FifoFillLevel, FifoWatermark,
        GyroConfig, GyroSelfCalibSelect, I2cWatchdogConfig, IbiStatus, Int1Status, Int2Status,
        IntLatchConfig, IntPinConfig, IoPadStrength, RawAccelRange, RawGyroRange, Register,
        SaturationReg, SensorInterruptMap, StatusReg, ACCEL_DATA_ADDR,
    },
    AccelMode, AccelRange, Bmi323Async, Bmi323Error, GyroMode, Measurement, MeasurementRaw3D,
    SelfCalibrateType, MAX_LOOPS,
};
use embedded_hal_async::{delay::DelayNs, i2c, spi};

/// Trait for asynchronous read and write operations.
#[allow(async_fn_in_trait)]
pub trait AsyncInterface: Interface {
    /// The error type for the interface.
    type Error;
    /// Write data to the device.
    async fn write(&mut self, data: &[u8]) -> Result<(), Self::Error>;
    /// Write to a register and read data from the device.
    async fn write_read(&mut self, address: u8, buffer: &mut [u8]) -> Result<(), Self::Error>;
}

impl<I2C, E> AsyncInterface for I2cInterface<I2C>
where
    I2C: i2c::I2c<Error = E>,
{
    type Error = E;

    async fn write(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        self.i2c.write(self.address, data).await
    }

    async fn write_read(&mut self, address: u8, buffer: &mut [u8]) -> Result<(), Self::Error> {
        let mut dummy = [0u8; 2];
        self.i2c
            .transaction(
                self.address,
                &mut [
                    i2c::Operation::Write(&[address]),
                    i2c::Operation::Read(&mut dummy),
                    i2c::Operation::Read(buffer),
                ],
            )
            .await?;
        trace!("I2C Read from {:#x}: {=[u8]:#x}", address, buffer);
        Ok(())
    }
}

impl<SPI, E> AsyncInterface for SpiInterface<SPI>
where
    SPI: spi::SpiDevice<Error = E>,
{
    type Error = E;

    async fn write(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        {
            trace!("SPI Write: {=[u8]:#x}", data);
        }
        self.spi.write(data).await
    }

    async fn write_read(&mut self, address: u8, buffer: &mut [u8]) -> Result<(), Self::Error> {
        let mut dummy = [0u8; 1];
        self.spi
            .transaction(&mut [
                spi::Operation::Write(&[address]),
                spi::Operation::Read(&mut dummy),
                spi::Operation::Read(buffer),
            ])
            .await?;
        trace!("SPI Read from {:#x}: {=[u8]:#x}", address, buffer);
        Ok(())
    }
}

/// Trait for reading and writing registers asynchronously.
pub(crate) trait AsyncRegister<IFACE>
where
    IFACE: AsyncInterface,
    Self: Register + Sized,
{
    async fn read_register(iface: &mut IFACE) -> Result<Self, IFACE::Error> {
        let mut data = [0u8; 2];
        iface.write_read(Self::ADDRESS, &mut data).await?;
        Ok(Self::from_u16(u16::from_le_bytes(data)))
    }

    async fn write_register(&self, iface: &mut IFACE) -> Result<(), IFACE::Error> {
        let mut buf = [0; 3];
        buf[0] = Self::ADDRESS;
        buf[1..].copy_from_slice(&self.to_u16().to_le_bytes());
        iface.write(&buf).await?;
        Ok(())
    }
}

macro_rules! impl_async_register {
    ($($reg:ty),+) => {
        $(
            impl<IFACE> AsyncRegister<IFACE> for $reg
            where
                IFACE: AsyncInterface,
            {}
        )+
    };
}

impl_async_register! {
    ErrorReg, StatusReg, SaturationReg, Int1Status, Int2Status,
    IbiStatus, FeatEngConfig, FeatEngIo0, FeatEngIoStat, FifoFillLevel, AccelConfig,
    GyroConfig, FifoWatermark, FifoConfig, FifoCtrl, IntPinConfig, IntLatchConfig,
    FeatureInterruptMap, SensorInterruptMap, FeatureEngineControl, FeatEngAddr,
    FeatureDataStatus, FeatureEngineStatus, IoPadStrength,
    I2cWatchdogConfig, Command, DeviceId, GyroSelfCalibSelect
}

impl<IFACE, E, D, M: RawMutex> Bmi323Async<IFACE, D, M>
where
    IFACE: AsyncInterface<Error = E>,
    D: DelayNs,
{
    /// Initialize the device.
    pub async fn init(&mut self) -> Result<(), Bmi323Error<E>> {
        {
            let (ref mut iface, _) = *self.core.lock().await;
            // Check device ID
            let device_id = DeviceId::read_register(iface).await?;
            if !device_id.validate() {
                return Err(Bmi323Error::InvalidDevice);
            }
        }
        self.reset().await
    }

    /// Resets the device.
    pub async fn reset(&mut self) -> Result<(), Bmi323Error<E>> {
        let (ref mut iface, ref mut delay) = *self.core.lock().await;
        // Reset device
        Command::SoftReset.write_register(iface).await?;
        delay.delay_ms(20).await;

        // Write configuration
        let (
            feature_engine_control,
            fifo_config,
            sensor_interrupt_map,
            i2c_watchdog_config,
            int_pin_config,
            accel_config,
            gyro_config,
        ) = self.config.get_registers();

        feature_engine_control.write_register(iface).await?;
        fifo_config.write_register(iface).await?;
        sensor_interrupt_map.write_register(iface).await?;
        i2c_watchdog_config.write_register(iface).await?;
        int_pin_config.write_register(iface).await?;
        accel_config.write_register(iface).await?;
        gyro_config.write_register(iface).await?;
        // Sync active range atomics so measure() uses what was written to hardware
        self.active_accel_range
            .store(self.config.raw_accel_range.into_bits(), Ordering::Relaxed);
        self.active_gyro_range
            .store(self.config.raw_gyro_range.into_bits(), Ordering::Relaxed);
        self.accel_down_ctr.store(0, Ordering::Relaxed);
        self.gyro_down_ctr.store(0, Ordering::Relaxed);
        self.accel_down_threshold = crate::compute_down_threshold(
            self.config.accel_odr().delay(),
            self.config.auto_range_hysteresis_us,
        );
        self.gyro_down_threshold = crate::compute_down_threshold(
            self.config.gyro_odr().delay(),
            self.config.auto_range_hysteresis_us,
        );

        let error = ErrorReg::read_register(iface).await?;
        if error.fatal() {
            Err(Bmi323Error::FatalError)
        } else if error.accel_conf() {
            Err(Bmi323Error::InvalidAccelConfig)
        } else if error.gyro_conf() {
            Err(Bmi323Error::InvalidGyroConfig)
        } else {
            Ok(())
        }
    }

    /// Starts continuous measurement mode, runs the provided async closure, then stops measurement mode.
    pub async fn start<'a: 'b, 'b, F, Fut>(&'a mut self, f: F) -> Result<(), Bmi323Error<E>>
    where
        F: FnOnce(&'b Self) -> Fut,
        Fut: Future<Output = Result<(), Bmi323Error<E>>>,
    {
        start(self).await?;
        f(self).await?;
        stop(self).await
    }

    /// Reads a measurement from the device.
    ///
    /// This method triggers the measurement if the sensor is not in
    /// continuous measurement mode.
    pub async fn measure(&self) -> Result<Measurement, Bmi323Error<E>> {
        if !self.running.load(Ordering::Relaxed) {
            return Err(Bmi323Error::NotReady);
        }
        let (ref mut iface, ref mut delay) = *self.core.lock().await;
        let status = if !self.config.irq_enabled {
            let mut ctr = MAX_LOOPS;
            loop {
                let status = StatusReg::read_register(iface).await?;
                if status.drdy_acc() || status.drdy_gyr() || status.drdy_temp() {
                    break status;
                }
                delay.delay_us(self.config.min_delay_us).await;
                ctr -= 1;
                if ctr == 0 {
                    return Err(Bmi323Error::NoDataAvailable);
                }
            }
        } else {
            StatusReg::read_register(iface).await?
        };
        trace!("Status: {=u16:#x}", status.to_u16());
        let read = status.drdy_acc() || status.drdy_gyr() || status.drdy_temp();
        if !read {
            return Err(Bmi323Error::NoDataAvailable);
        }
        let mut buf = [0u8; 18];
        iface.write_read(ACCEL_DATA_ADDR, &mut buf).await?;

        // Load active ranges — may differ from config when auto-ranging is active
        let cur_accel_range =
            RawAccelRange::from_u8(self.active_accel_range.load(Ordering::Relaxed));
        let cur_gyro_range = RawGyroRange::from_u8(self.active_gyro_range.load(Ordering::Relaxed));

        let accel_data = if status.drdy_acc() {
            let meas = MeasurementRaw3D::new()
                .with_x(i16::from_le_bytes([buf[0], buf[1]]))
                .with_y(i16::from_le_bytes([buf[2], buf[3]]))
                .with_z(i16::from_le_bytes([buf[4], buf[5]]))
                .with_kind(crate::Measurement3DKind::Accel(cur_accel_range));
            Some(meas)
        } else {
            None
        };
        let gyro_data = if status.drdy_gyr() {
            let meas = MeasurementRaw3D::new()
                .with_x(i16::from_le_bytes([buf[6], buf[7]]))
                .with_y(i16::from_le_bytes([buf[8], buf[9]]))
                .with_z(i16::from_le_bytes([buf[10], buf[11]]))
                .with_kind(crate::Measurement3DKind::Gyro(cur_gyro_range));
            Some(meas)
        } else {
            None
        };

        // Auto-ranging: adjust sensor range based on saturation and signal level
        if self.config.accel_range() == AccelRange::Auto {
            let sat = SaturationReg::read_register(iface).await?;
            let mut range_changed = false;

            // Step up if saturated — measurement is clipped and must be discarded
            if sat.acc_x() || sat.acc_y() || sat.acc_z() {
                if let Some(next) = cur_accel_range.step_up() {
                    info!("Accel saturated: range {:?} -> {:?}", cur_accel_range, next);
                    let mut accel_cfg = AccelConfig::read_register(iface).await?;
                    accel_cfg.set_range(next);
                    accel_cfg.write_register(iface).await?;
                    self.active_accel_range
                        .store(next.into_bits(), Ordering::Relaxed);
                    self.accel_down_ctr.store(0, Ordering::Relaxed);
                    range_changed = true;
                }
            }
            if sat.gyr_x() || sat.gyr_y() || sat.gyr_z() {
                if let Some(next) = cur_gyro_range.step_up() {
                    info!("Gyro saturated: range {:?} -> {:?}", cur_gyro_range, next);
                    let mut gyro_cfg = GyroConfig::read_register(iface).await?;
                    gyro_cfg.set_range(next);
                    gyro_cfg.write_register(iface).await?;
                    self.active_gyro_range
                        .store(next.into_bits(), Ordering::Relaxed);
                    self.gyro_down_ctr.store(0, Ordering::Relaxed);
                    range_changed = true;
                }
            }
            if range_changed {
                return Err(Bmi323Error::AutoRangeAdjusted);
            }

            // Step down if under-utilizing: peak < 25% of full scale on all axes.
            // 25% of i16::MAX (32767) ≈ 8192 counts.
            // A hysteresis counter must reach the pre-computed sample count before stepping down.
            const DOWN_THRESHOLD: u16 = 8192;
            let accel_hysteresis = self.accel_down_threshold;
            let gyro_hysteresis = self.gyro_down_threshold;
            if let Some(accel) = accel_data.as_ref() {
                let peak = accel
                    .x()
                    .unsigned_abs()
                    .max(accel.y().unsigned_abs())
                    .max(accel.z().unsigned_abs());
                if peak < DOWN_THRESHOLD {
                    let ctr = self
                        .accel_down_ctr
                        .fetch_add(1, Ordering::Relaxed)
                        .saturating_add(1);
                    if ctr >= accel_hysteresis {
                        if let Some(next) = cur_accel_range.step_down() {
                            info!(
                                "Accel under-range: range {:?} -> {:?}",
                                cur_accel_range, next
                            );
                            let mut accel_cfg = AccelConfig::read_register(iface).await?;
                            accel_cfg.set_range(next);
                            accel_cfg.write_register(iface).await?;
                            self.active_accel_range
                                .store(next.into_bits(), Ordering::Relaxed);
                            self.accel_down_ctr.store(0, Ordering::Relaxed);
                        }
                    }
                } else {
                    self.accel_down_ctr.store(0, Ordering::Relaxed);
                }
            }
            if let Some(gyro) = gyro_data.as_ref() {
                let peak = gyro
                    .x()
                    .unsigned_abs()
                    .max(gyro.y().unsigned_abs())
                    .max(gyro.z().unsigned_abs());
                if peak < DOWN_THRESHOLD {
                    let ctr = self
                        .gyro_down_ctr
                        .fetch_add(1, Ordering::Relaxed)
                        .saturating_add(1);
                    if ctr >= gyro_hysteresis {
                        if let Some(next) = cur_gyro_range.step_down() {
                            info!("Gyro under-range: range {:?} -> {:?}", cur_gyro_range, next);
                            let mut gyro_cfg = GyroConfig::read_register(iface).await?;
                            gyro_cfg.set_range(next);
                            gyro_cfg.write_register(iface).await?;
                            self.active_gyro_range
                                .store(next.into_bits(), Ordering::Relaxed);
                            self.gyro_down_ctr.store(0, Ordering::Relaxed);
                        }
                    }
                } else {
                    self.gyro_down_ctr.store(0, Ordering::Relaxed);
                }
            }
        }

        let temp_data = if status.drdy_temp() {
            Some(crate::TemperatureMeasurement(i16::from_le_bytes([
                buf[12], buf[13],
            ])))
        } else {
            None
        };
        let timestamp =
            crate::TimestampMeasurement(u32::from_le_bytes([buf[14], buf[15], buf[16], buf[17]]));
        Ok(Measurement {
            accel: accel_data,
            gyro: gyro_data,
            temp: temp_data,
            timestamp,
        })
    }

    /// Calibrate the gyroscope.
    ///
    /// Note: The device must be kept still during calibration.
    pub async fn calibrate(&mut self, what: SelfCalibrateType) -> Result<(), Bmi323Error<E>> {
        if self.running.load(Ordering::Relaxed) {
            return Err(Bmi323Error::Busy);
        }
        let (ref mut iface, ref mut delay) = *self.core.lock().await;
        let feature_eng_stat = FeatEngIo0::read_register(iface).await?;
        if !(feature_eng_stat.errors() == FeatureIo1Error::Active
            || feature_eng_stat.errors() == FeatureIo1Error::Activated
            || feature_eng_stat.errors() == FeatureIo1Error::NoError)
        {
            return Err(Bmi323Error::RestartRequired);
        }
        let irq_config = IntPinConfig::new()
            .with_int1_output_en(false)
            .with_int2_output_en(false);
        let accel_cfg = AccelConfig::new()
            .with_odr(crate::OutputDataRate::Hz100)
            .with_range(crate::RawAccelRange::G2)
            .with_mode(AccelMode::HighPerformance);
        let gyro_cfg = GyroConfig::new()
            .with_odr(crate::OutputDataRate::Hz100)
            .with_range(crate::RawGyroRange::Dps125)
            .with_mode(GyroMode::HighPerformance);
        irq_config.write_register(iface).await?;
        accel_cfg.write_register(iface).await?;
        gyro_cfg.write_register(iface).await?;
        GyroSelfCalibSelect::new()
            .with_sensitivity(what.sensitivity())
            .with_offset(what.offset())
            .with_apply(true)
            .write_register(iface)
            .await?;
        delay.delay_ms(50).await;
        let errors = ErrorReg::read_register(iface).await?;
        if errors.fatal() || errors.accel_conf() || errors.gyro_conf() {
            return Err(Bmi323Error::FatalError);
        }

        warn!("Starting gyro calibration, keep the device still...");
        Command::GyroSelfCalib.write_register(iface).await?;
        delay.delay_ms(350).await;

        #[cfg(feature = "defmt-messages")]
        {
            debug!("Gyro calibration in progress...");
            let feat_eng_io0 = FeatEngIo0::read_register(iface).await?;
            debug!("Feature Engine IO0: {=u16:#b}", feat_eng_io0.to_u16());
        }
        delay.delay_ms(80).await;
        let mut ctr = MAX_LOOPS;
        loop {
            let feat_eng_io0 = FeatEngIo0::read_register(iface).await?;
            debug!("Feature Engine IO0: {=u16:#b}", feat_eng_io0.to_u16());
            if feat_eng_io0.self_proc_done() {
                break;
            }
            delay.delay_ms(5).await;
            ctr -= 1;
            if ctr == 0 {
                return Err(Bmi323Error::SelfCalTimedOut);
            }
        }

        debug!("Gyro calibration complete.");
        let (_, _, _, _, irq_config, gyro_cfg, accel_cfg) = self.config.get_registers();
        irq_config.write_register(iface).await?;
        gyro_cfg.write_register(iface).await?;
        accel_cfg.write_register(iface).await?;
        let error = ErrorReg::read_register(iface).await?;
        if error.sensor_fatal() {
            Err(Bmi323Error::FatalError)
        } else {
            Ok(())
        }
    }

    /// Check if calibration is applied.
    pub async fn calibrated(&self) -> Result<bool, Bmi323Error<E>> {
        let (ref mut iface, _) = *self.core.lock().await;
        let gyro_self_calib = GyroSelfCalibSelect::read_register(iface).await?;
        Ok(gyro_self_calib.apply())
    }
}

async fn stop<I, D, E, M>(sensor: &Bmi323Async<I, D, M>) -> Result<(), Bmi323Error<E>>
where
    I: AsyncInterface<Error = E>,
    D: DelayNs,
    M: RawMutex,
{
    if !sensor.running.load(Ordering::Relaxed) {
        return Ok(());
    }
    let (ref mut iface, _) = *sensor.core.lock().await;
    // Disable sensors
    let mut accel_config = AccelConfig::read_register(iface).await?;
    let mut gyro_config = GyroConfig::read_register(iface).await?;
    accel_config.set_mode(AccelMode::Off);
    gyro_config.set_mode(GyroMode::Off);
    accel_config.write_register(iface).await?;
    gyro_config.write_register(iface).await?;
    sensor.running.store(false, Ordering::Relaxed);
    Ok(())
}

async fn start<I, D, E, M>(sensor: &Bmi323Async<I, D, M>) -> Result<(), Bmi323Error<E>>
where
    I: AsyncInterface<Error = E>,
    D: DelayNs,
    M: RawMutex,
{
    if sensor.running.load(Ordering::Relaxed) {
        return Ok(());
    }
    let (ref mut iface, _) = *sensor.core.lock().await;
    if sensor.config.sensors_enabled == AccGyroEnabled::None {
        return Err(Bmi323Error::NoSensorsEnabled);
    }
    // Enable sensors
    let mut accel_config = AccelConfig::read_register(iface).await?;
    let mut gyro_config = GyroConfig::read_register(iface).await?;
    if sensor.config.sensors_enabled.is_accel_enabled() {
        accel_config.set_mode(sensor.config.accel_mode());
    }
    if sensor.config.sensors_enabled.is_gyro_enabled() {
        gyro_config.set_mode(sensor.config.gyro_mode());
    }
    accel_config.write_register(iface).await?;
    gyro_config.write_register(iface).await?;
    #[cfg(feature = "defmt-messages")]
    {
        let accel_config = AccelConfig::read_register(iface).await?;
        let gyro_config = GyroConfig::read_register(iface).await?;
        debug!("Accel Config: {:#b}", accel_config.to_u16());
        debug!("Gyro Config: {:#b}", gyro_config.to_u16());
    }
    sensor.running.store(true, Ordering::Relaxed);
    Ok(())
}
