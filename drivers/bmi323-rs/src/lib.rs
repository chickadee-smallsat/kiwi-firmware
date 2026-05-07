#![no_std]
#![deny(missing_docs)]
//! Embassy-compatible driver for the BMI323 IMU
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU8};
#[cfg(feature = "async")]
use embassy_sync::{blocking_mutex::raw::RawMutex, mutex::Mutex};

use bitfield_struct::bitfield;

pub use crate::{
    config::{Bmi323Config, IrqPinConfig},
    interface::{I2cInterface, SpiInterface},
    registers::{
        AccelMode, AccelRange, AveragingSamples, Bandwidth, GyroMode, GyroRange, IrqMap,
        OutputDataRate, RawAccelRange, RawGyroRange,
    },
};

#[cfg(feature = "defmt-messages")]
#[allow(unused_imports)]
use defmt::{debug, error, info, trace, warn};

// A replacement for the defmt logging macros, when defmt is not provided
#[cfg(not(feature = "defmt-messages"))]
mod log_impl {
    #![allow(unused_macros)]
    #![allow(unused_imports)]
    // Macros are defined as _ to avoid conflicts with built-in attribute
    // names
    macro_rules! _trace {
        ($($arg:tt)*) => {};
    }
    macro_rules! _debug {
        ($($arg:tt)*) => {};
    }
    macro_rules! _info {
        ($($arg:tt)*) => {};
    }
    macro_rules! _warn {
        ($($arg:tt)*) => {};
    }
    macro_rules! _error {
        ($($arg:tt)*) => {};
    }
    pub(crate) use _debug as debug;
    pub(crate) use _error as error;
    pub(crate) use _info as info;
    pub(crate) use _trace as trace;
    pub(crate) use _warn as warn;
}
#[cfg(not(feature = "defmt-messages"))]
use log_impl::*;

#[cfg(feature = "async")]
mod r#async;
mod config;
mod interface;
mod registers;
#[cfg(feature = "sync")]
mod sync;

/// Default I2C address for the BMI323 sensor
pub const DEFAULT_I2C_ADDRESS: u8 = 0x68;

#[cfg(feature = "async")]
pub use crate::r#async::AsyncInterface;
#[cfg(feature = "sync")]
pub use crate::sync::SyncInterface;

/// BMI323 6-axis IMU device
#[cfg(feature = "sync")]
pub struct Bmi323Sync<IFACE, D> {
    iface: IFACE,
    delay: D,
    /// Configuration for the sensor
    pub config: Bmi323Config,
    running: AtomicBool,
    active_accel_range: AtomicU8,
    active_gyro_range: AtomicU8,
    /// Counts consecutive measurements where the accel signal is below the step-down threshold.
    accel_down_ctr: AtomicU16,
    /// Counts consecutive measurements where the gyro signal is below the step-down threshold.
    gyro_down_ctr: AtomicU16,
    /// Pre-computed step-down hysteresis sample count for the accelerometer.
    accel_down_threshold: u16,
    /// Pre-computed step-down hysteresis sample count for the gyroscope.
    gyro_down_threshold: u16,
}

#[cfg(feature = "sync")]
impl<I2C, D> Bmi323Sync<I2cInterface<I2C>, D> {
    /// Create a new instance of the [`Bmi323Sync`] device.
    ///
    /// # Arguments
    /// * `i2c` - The I2C peripheral to use.
    /// * `address` - The I2C address of the device. Use `DEFAULT_I2C_ADDRESS`.
    /// * `config` - The configuration to use.
    /// * `delay` - A delay provider to use for initialization.
    ///
    /// # Errors
    /// * Returns [`Bmi323Error::Comm`] if there is an error communicating with the device.
    /// * Returns [`Bmi323Error::InvalidDevice`] if the device ID is incorrect.
    pub fn new_with_i2c(i2c: I2C, address: u8, config: Bmi323Config, delay: D) -> Self {
        let raw_accel = config.raw_accel_range.into_bits();
        let raw_gyro = config.raw_gyro_range.into_bits();
        let accel_thr =
            compute_down_threshold(config.accel_odr().delay(), config.auto_range_hysteresis_us);
        let gyro_thr =
            compute_down_threshold(config.gyro_odr().delay(), config.auto_range_hysteresis_us);
        Self {
            iface: I2cInterface { i2c, address },
            delay,
            config,
            running: AtomicBool::new(false),
            active_accel_range: AtomicU8::new(raw_accel),
            active_gyro_range: AtomicU8::new(raw_gyro),
            accel_down_ctr: AtomicU16::new(0),
            gyro_down_ctr: AtomicU16::new(0),
            accel_down_threshold: accel_thr,
            gyro_down_threshold: gyro_thr,
        }
    }
}

#[cfg(feature = "sync")]
impl<SPI, D> Bmi323Sync<SpiInterface<SPI>, D> {
    /// Create a new instance of the [`Bmi323Sync`] device.
    ///
    /// # Arguments
    /// * `spi` - The SPI peripheral to use.
    /// * `config` - The configuration to use.
    /// * `delay` - A delay provider to use for initialization.
    ///
    /// # Errors
    /// * Returns [`Bmi323Error::Comm`] if there is an error communicating with the device.
    /// * Returns [`Bmi323Error::InvalidDevice`] if the device ID is incorrect.
    pub async fn new_with_spi(spi: SPI, config: Bmi323Config, delay: D) -> Self {
        let raw_accel = config.raw_accel_range.into_bits();
        let raw_gyro = config.raw_gyro_range.into_bits();
        let accel_thr =
            compute_down_threshold(config.accel_odr().delay(), config.auto_range_hysteresis_us);
        let gyro_thr =
            compute_down_threshold(config.gyro_odr().delay(), config.auto_range_hysteresis_us);
        Self {
            iface: SpiInterface { spi },
            delay,
            config,
            running: AtomicBool::new(false),
            active_accel_range: AtomicU8::new(raw_accel),
            active_gyro_range: AtomicU8::new(raw_gyro),
            accel_down_ctr: AtomicU16::new(0),
            gyro_down_ctr: AtomicU16::new(0),
            accel_down_threshold: accel_thr,
            gyro_down_threshold: gyro_thr,
        }
    }
}

/// BMI323 6-axis IMU device
#[cfg(feature = "async")]
pub struct Bmi323Async<IFACE, D, M: RawMutex> {
    core: Mutex<M, (IFACE, D)>,
    /// Configuration for the sensor
    pub config: Bmi323Config,
    running: AtomicBool,
    active_accel_range: AtomicU8,
    active_gyro_range: AtomicU8,
    /// Counts consecutive measurements where the accel signal is below the step-down threshold.
    accel_down_ctr: AtomicU16,
    /// Counts consecutive measurements where the gyro signal is below the step-down threshold.
    gyro_down_ctr: AtomicU16,
    /// Pre-computed step-down hysteresis sample count for the accelerometer.
    accel_down_threshold: u16,
    /// Pre-computed step-down hysteresis sample count for the gyroscope.
    gyro_down_threshold: u16,
}

#[cfg(feature = "async")]
impl<I2C, D, M: RawMutex> Bmi323Async<I2cInterface<I2C>, D, M> {
    /// Create a new instance of the [`Bmi323Async`] device.
    ///
    /// # Arguments
    /// * `i2c` - The I2C peripheral to use.
    /// * `address` - The I2C address of the device. Use `DEFAULT_I2C_ADDRESS`.
    /// * `config` - The configuration to use.
    /// * `delay` - A delay provider to use for initialization.
    ///
    /// # Errors
    /// * Returns [`Bmi323Error::Comm`] if there is an error communicating with the device.
    /// * Returns [`Bmi323Error::InvalidDevice`] if the device ID is incorrect.
    pub fn new_with_i2c(i2c: I2C, address: u8, config: Bmi323Config, delay: D) -> Self {
        let raw_accel = config.raw_accel_range.into_bits();
        let raw_gyro = config.raw_gyro_range.into_bits();
        let accel_thr =
            compute_down_threshold(config.accel_odr().delay(), config.auto_range_hysteresis_us);
        let gyro_thr =
            compute_down_threshold(config.gyro_odr().delay(), config.auto_range_hysteresis_us);
        Self {
            core: Mutex::new((I2cInterface { i2c, address }, delay)),
            config,
            running: AtomicBool::new(false),
            active_accel_range: AtomicU8::new(raw_accel),
            active_gyro_range: AtomicU8::new(raw_gyro),
            accel_down_ctr: AtomicU16::new(0),
            gyro_down_ctr: AtomicU16::new(0),
            accel_down_threshold: accel_thr,
            gyro_down_threshold: gyro_thr,
        }
    }
}

#[cfg(feature = "async")]
impl<SPI, D, M: RawMutex> Bmi323Async<SpiInterface<SPI>, D, M> {
    /// Create a new instance of the [`Bmi323Async`] device.
    ///
    /// # Arguments
    /// * `spi` - The SPI peripheral to use.
    /// * `config` - The configuration to use.
    /// * `delay` - A delay provider to use for initialization.
    ///
    /// # Errors
    /// * Returns [`Bmi323Error::Comm`] if there is an error communicating with the device.
    /// * Returns [`Bmi323Error::InvalidDevice`] if the device ID is incorrect.
    pub async fn new_with_spi(spi: SPI, config: Bmi323Config, delay: D) -> Self {
        let raw_accel = config.raw_accel_range.into_bits();
        let raw_gyro = config.raw_gyro_range.into_bits();
        let accel_thr =
            compute_down_threshold(config.accel_odr().delay(), config.auto_range_hysteresis_us);
        let gyro_thr =
            compute_down_threshold(config.gyro_odr().delay(), config.auto_range_hysteresis_us);
        Self {
            core: Mutex::new((SpiInterface { spi }, delay)),
            config,
            running: AtomicBool::new(false),
            active_accel_range: AtomicU8::new(raw_accel),
            active_gyro_range: AtomicU8::new(raw_gyro),
            accel_down_ctr: AtomicU16::new(0),
            gyro_down_ctr: AtomicU16::new(0),
            accel_down_threshold: accel_thr,
            gyro_down_threshold: gyro_thr,
        }
    }
}

/// Errors that can occur when interacting with the BMI323 sensor.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Bmi323Error<CommError> {
    /// I2C communication error
    Comm(CommError),
    /// Fatal error reported by the sensor
    FatalError,
    /// Invalid accelerometer configuration
    InvalidAccelConfig,
    /// Invalid gyroscope configuration
    InvalidGyroConfig,
    /// Invalid device (wrong device ID)
    InvalidDevice,
    /// Driver not ready (e.g., measurement not started)
    NotReady,
    /// No new data available to read
    NoDataAvailable,
    /// Driver is currently busy (e.g., during calibration)
    Busy,
    /// Operation requires a restart of the sensor
    RestartRequired,
    /// Self-calibration process timed out
    SelfCalTimedOut,
    /// No sensors (accelerometer or gyroscope) are enabled
    NoSensorsEnabled,
    /// Invalid access
    InvalidAccess,
    /// Measurement discarded because auto-ranging adjusted the sensor range.
    /// The caller should re-read to obtain a valid measurement at the new range.
    AutoRangeAdjusted,
}

impl<CommError> From<CommError> for Bmi323Error<CommError> {
    fn from(err: CommError) -> Self {
        Bmi323Error::Comm(err)
    }
}

/// Raw measurement data from the sensor
#[bitfield(u64)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct MeasurementRaw3D {
    #[bits(16)]
    pub x: i16,
    #[bits(16)]
    pub y: i16,
    #[bits(16)]
    pub z: i16,
    #[bits(8)]
    pub kind: Measurement3DKind,
    #[bits(8)]
    _reserved: u8,
}

impl MeasurementRaw3D {
    /// Convert the raw measurement data to floating point values in physical units
    pub fn float(&self) -> (f32, f32, f32) {
        let sensitivity = match self.kind() {
            Measurement3DKind::Accel(range) => range.sensitivity(),
            Measurement3DKind::Gyro(range) => range.sensitivity(),
        };
        (
            self.x() as f32 / sensitivity,
            self.y() as f32 / sensitivity,
            self.z() as f32 / sensitivity,
        )
    }
}

/// Raw temperature measurement data from the sensor
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct TemperatureMeasurement(pub i16);

impl TemperatureMeasurement {
    /// Convert the raw temperature measurement to degrees Celsius
    pub fn celcius(&self) -> f32 {
        self.0 as f32 / 512.0 + 23.0
    }
}

#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Raw timestamp data from the sensor
pub struct TimestampMeasurement(pub u32);

/// Sensor readout
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Measurement {
    /// Accelerometer data, if enabled
    pub accel: Option<MeasurementRaw3D>,
    /// Gyroscope data, if enabled
    pub gyro: Option<MeasurementRaw3D>,
    /// Temperature data, if enabled
    pub temp: Option<TemperatureMeasurement>,
    /// Timestamp data, if enabled
    pub timestamp: TimestampMeasurement,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// 3D Measurement type
pub enum Measurement3DKind {
    /// Accelerometer measurement
    Accel(RawAccelRange),
    /// Gyroscope measurement
    Gyro(RawGyroRange),
}

impl Measurement3DKind {
    pub(crate) const fn into_bits(self) -> u8 {
        match self {
            Measurement3DKind::Accel(range) => range as u8,
            Measurement3DKind::Gyro(range) => 0x80 | (range as u8),
        }
    }

    pub(crate) const fn from_bits(bits: u8) -> Self {
        match bits & 0x80 {
            0x00 => Measurement3DKind::Accel(RawAccelRange::from_u8(bits & 0x7F)),
            0x80 => Measurement3DKind::Gyro(RawGyroRange::from_u8(bits & 0x7F)),
            _ => unreachable!(),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Type of gyroscope self-calibration
pub enum SelfCalibrateType {
    /// Calibrate only sensitivity
    Sensitivity = 1,
    /// Calibrate only offset
    Offset = 2,
    /// Calibrate both sensitivity and offset
    Both = 3,
}

impl SelfCalibrateType {
    pub(crate) const fn sensitivity(&self) -> bool {
        matches!(
            self,
            SelfCalibrateType::Sensitivity | SelfCalibrateType::Both
        )
    }
    pub(crate) const fn offset(&self) -> bool {
        matches!(self, SelfCalibrateType::Offset | SelfCalibrateType::Both)
    }
}

const MAX_LOOPS: usize = 100;
/// Compute the number of consecutive samples needed for the step-down hysteresis, given
/// the sensor period in microseconds and the hysteresis duration in microseconds.
/// Result is saturating-clamped to [`u16::MAX`].
pub(crate) const fn compute_down_threshold(period_us: u32, hysteresis_us: u32) -> u16 {
    let samples = hysteresis_us.div_ceil(period_us);
    if samples > u16::MAX as u32 {
        u16::MAX
    } else {
        samples as u16
    }
}
