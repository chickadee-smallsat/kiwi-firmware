#![no_std]
#![warn(missing_docs)]
//! Driver for the MS5607 pressure sensor.

use crate::interface::{I2cInterface, SpiInterface};

#[cfg(feature = "async")]
mod async_impl;
#[cfg(feature = "sync")]
mod blocking_impl;
mod details;
mod interface;

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

/// Address pin configuration
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Address {
    /// CSB is pulled high
    CsbHigh = 0x76,
    /// CSB is pulled low
    CsbLow = 0x77,
}

impl TryFrom<u8> for Address {
    type Error = &'static str;
    #[inline]
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x76 => Ok(Self::CsbHigh),
            0x77 => Ok(Self::CsbLow),
            _ => Err("Invalid address"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Oversampling configuration for the sensor
pub enum Oversampling {
    /// No oversampling, 256 samples per measurement
    Osr256 = 0,
    /// 512 samples per measurement
    Osr512 = 2,
    /// 1024 samples per measurement
    Osr1024 = 4,
    /// 2048 samples per measurement
    Osr2048 = 6,
    /// 4096 samples per measurement
    #[default]
    Osr4096 = 8,
}

/// MS5607 device
pub struct Ms5607<IFACE, D> {
    iface: IFACE,
    delay: D,
    osr: Oversampling,
    calibration: Option<details::Calibration>,
}

impl<SPI, D> Ms5607<SpiInterface<SPI>, D> {
    /// Create a new driver instance with the given SPI interface and delay provider
    pub fn new_spi(spi: SPI, delay: D, oversampling: Oversampling) -> Self {
        Self {
            iface: SpiInterface { spi },
            delay,
            osr: oversampling,
            calibration: None,
        }
    }
}

impl<I2C, D> Ms5607<I2cInterface<I2C>, D> {
    /// Create a new driver instance with the given I2C interface, address, and delay provider
    pub fn new_i2c(i2c: I2C, address: Address, delay: D, oversampling: Oversampling) -> Self {
        Self {
            iface: I2cInterface {
                i2c,
                address: address as u8,
            },
            delay,
            osr: oversampling,
            calibration: None,
        }
    }

    /// Get the I2C address of the device
    pub const fn address(&self) -> u8 {
        self.iface.address
    }
}

/// Errors that can occur when interacting with the MS5607 sensor.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Error<CommError> {
    /// Communication error
    Comm(CommError),
    /// CRC check failed
    Crc,
    /// Device not initialized.
    /// Call `init()` to initialize the device before reading measurements.
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
