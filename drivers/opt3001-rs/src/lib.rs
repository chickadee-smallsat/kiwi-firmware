#![no_std]
#![warn(missing_docs)]
#![cfg_attr(not(any(feature = "sync", feature = "async")), allow(dead_code))]
//! Driver for the Texas Instruments OPT3001 ambient light sensor.

use crate::interface::I2cInterface;

#[cfg(feature = "async")]
mod async_impl;
#[cfg(feature = "sync")]
mod blocking_impl;
mod details;
mod interface;

#[cfg(feature = "defmt-messages")]
#[allow(unused_imports)]
use defmt::{debug, error, info, trace, warn};

#[cfg(not(feature = "defmt-messages"))]
mod log_impl {
    #![allow(unused_macros)]
    #![allow(unused_imports)]
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

/// ADDR pin configuration for the OPT3001 I2C address.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Address {
    /// ADDR is connected to GND.
    Gnd = 0x44,
    /// ADDR is connected to VDD.
    Vdd = 0x45,
    /// ADDR is connected to SDA.
    Sda = 0x46,
    /// ADDR is connected to SCL.
    Scl = 0x47,
}

impl TryFrom<u8> for Address {
    type Error = &'static str;

    #[inline]
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x44 => Ok(Self::Gnd),
            0x45 => Ok(Self::Vdd),
            0x46 => Ok(Self::Sda),
            0x47 => Ok(Self::Scl),
            _ => Err("Invalid address"),
        }
    }
}

/// Full-scale range configuration.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Range {
    /// 40.95 lux full-scale range.
    FullScale40Lux = 0x0,
    /// 81.90 lux full-scale range.
    FullScale80Lux = 0x1,
    /// 163.80 lux full-scale range.
    FullScale160Lux = 0x2,
    /// 327.60 lux full-scale range.
    FullScale320Lux = 0x3,
    /// 655.20 lux full-scale range.
    FullScale650Lux = 0x4,
    /// 1310.40 lux full-scale range.
    FullScale1kLux = 0x5,
    /// 2620.80 lux full-scale range.
    FullScale2kLux = 0x6,
    /// 5241.60 lux full-scale range.
    FullScale5kLux = 0x7,
    /// 10483.20 lux full-scale range.
    FullScale10kLux = 0x8,
    /// 20966.40 lux full-scale range.
    FullScale20kLux = 0x9,
    /// 41932.80 lux full-scale range.
    FullScale40kLux = 0xA,
    /// 83865.60 lux full-scale range.
    FullScale80kLux = 0xB,
    /// Automatically selected full-scale range.
    #[default]
    Auto = 0xC,
}

/// Conversion time configuration.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum ConversionTime {
    /// 100 ms conversion time.
    Ms100 = 0,
    /// 800 ms conversion time.
    #[default]
    Ms800 = 1,
}

/// Conversion mode configuration.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Mode {
    /// Low-power shutdown mode.
    #[default]
    Shutdown = 0b00,
    /// Single-shot conversion mode.
    SingleShot = 0b01,
    /// Continuous conversion mode.
    Continuous = 0b10,
}

/// Interrupt output polarity.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum InterruptPolarity {
    /// ALERT pin is active low.
    #[default]
    ActiveLow = 0,
    /// ALERT pin is active high.
    ActiveHigh = 1,
}

/// Number of fault events required before asserting the interrupt output.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum FaultCount {
    /// One fault event.
    #[default]
    One = 0b00,
    /// Two consecutive fault events.
    Two = 0b01,
    /// Four consecutive fault events.
    Four = 0b10,
    /// Eight consecutive fault events.
    Eight = 0b11,
}

/// OPT3001 configuration register fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Config {
    /// Full-scale range.
    pub range: Range,
    /// Conversion time.
    pub conversion_time: ConversionTime,
    /// Conversion mode.
    pub mode: Mode,
    /// Whether limit flags latch until the configuration register is read.
    pub latch: bool,
    /// Interrupt output polarity.
    pub interrupt_polarity: InterruptPolarity,
    /// Mask the exponent bits in limit comparison registers.
    pub mask_exponent: bool,
    /// Fault count before interrupt assertion.
    pub fault_count: FaultCount,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            range: Range::Auto,
            conversion_time: ConversionTime::Ms800,
            mode: Mode::Shutdown,
            latch: true,
            interrupt_polarity: InterruptPolarity::ActiveLow,
            mask_exponent: false,
            fault_count: FaultCount::One,
        }
    }
}

/// OPT3001 ambient light sensor device.
pub struct Opt3001<IFACE, D> {
    iface: IFACE,
    delay: D,
    config: Config,
    initialized: bool,
}

impl<I2C, D> Opt3001<I2cInterface<I2C>, D> {
    /// Create a new driver instance with the given I2C interface, address, delay, and config.
    pub fn new_i2c(i2c: I2C, address: Address, delay: D, config: Config) -> Self {
        Self {
            iface: I2cInterface {
                i2c,
                address: address as u8,
            },
            delay,
            config,
            initialized: false,
        }
    }

    /// Get the I2C address of the device.
    pub const fn address(&self) -> u8 {
        self.iface.address
    }
}

impl<IFACE, D> Opt3001<IFACE, D> {
    /// Destroy the driver and return the underlying interface and delay provider.
    pub fn destroy(self) -> (IFACE, D) {
        (self.iface, self.delay)
    }

    /// Get the cached configuration.
    pub const fn config(&self) -> Config {
        self.config
    }
}

/// Errors that can occur when interacting with the OPT3001 sensor.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Error<CommError> {
    /// Communication error.
    Comm(CommError),
    /// Manufacturer ID register did not contain `0x5449`.
    InvalidManufacturerId(u16),
    /// Device ID register did not contain `0x3001`.
    InvalidDeviceId(u16),
    /// Result register contains an invalid exponent.
    InvalidResult,
    /// A conversion result was requested before the sensor reported readiness.
    NotReady,
    /// Device not initialized. Call `init()` before reading measurements.
    NotInitialized,
}

impl<CommError> From<CommError> for Error<CommError> {
    fn from(value: CommError) -> Self {
        Self::Comm(value)
    }
}

#[cfg(feature = "async")]
pub use async_impl::{AsyncInterface, AsyncIo};
#[cfg(feature = "sync")]
pub use blocking_impl::{SyncInterface, SyncIo};
#[cfg(not(feature = "float"))]
pub use details::MeasurementRaw as Measurement;
#[cfg(feature = "float")]
pub use details::MeasurementUnit as Measurement;
pub use details::Status;
#[cfg(feature = "float")]
pub use uom;
