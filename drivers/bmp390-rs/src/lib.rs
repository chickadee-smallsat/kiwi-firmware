#![no_std]
#![deny(missing_docs)]
//! `no_std`-compatible driver for the BMP390 barometric pressure and temperature sensor
use core::sync::atomic::AtomicBool;

#[cfg(feature = "async")]
use embassy_sync::{blocking_mutex::raw::RawMutex, mutex::Mutex};

pub use crate::{
    config::{Bmp390Config, IrqPinConfig},
    interface::{I2cInterface, SpiInterface},
    registers::{IIRFilterConfig, OutputDataRate, Oversampling, SensorMode},
};

#[cfg(feature = "async")]
mod r#async;
mod config;
mod conversion;
mod interface;
mod registers;
#[cfg(feature = "sync")]
mod sync;

pub use uom::si::f32::{Length, Pressure, ThermodynamicTemperature};
pub use uom::si::length::{foot, meter};
pub use uom::si::pressure::{hectopascal, millibar, pascal};
pub use uom::si::thermodynamic_temperature::degree_celsius;

/// Default I2C address for the BMP390 sensor
pub const DEFAULT_I2C_ADDRESS: u8 = 0x77;

#[cfg(feature = "async")]
pub use crate::r#async::AsyncInterface;
#[cfg(feature = "sync")]
pub use crate::sync::SyncInterface;

/// BMP390 with blocking interface
#[cfg(feature = "sync")]
pub struct Bmp390Sync<IFACE, D> {
    /// Interface to communicate with the sensor
    iface: IFACE,
    delay: D,
    /// Configuration for the sensor
    pub config: Bmp390Config,
    running: AtomicBool,
}

#[cfg(feature = "sync")]
impl<IFACE, D> Bmp390Sync<IFACE, D> {
    /// Destroy the device and return the underlying I2C peripheral and delay provider.
    pub fn destroy(self) -> (IFACE, D) {
        (self.iface, self.delay)
    }
}

#[cfg(feature = "sync")]
impl<I2C, D> Bmp390Sync<I2cInterface<I2C>, D> {
    /// Create a new instance of the [`Bmp390Sync`] device.
    ///
    /// # Arguments
    /// * `i2c` - The I2C peripheral to use.
    /// * `address` - The I2C address of the device. Use `DEFAULT_I2C_ADDRESS`.
    /// * `config` - The configuration to use.
    /// * `delay` - A delay provider to use for initialization.
    ///
    /// # Errors
    /// * Returns [`Bmp390Error::Comm`] if there is an error communicating with the device.
    /// * Returns [`Bmp390Error::InvalidDevice`] if the device ID is incorrect.
    pub fn new_with_i2c(i2c: I2C, address: u8, config: Bmp390Config, delay: D) -> Self {
        Self {
            iface: I2cInterface { i2c, address },
            delay,
            config,
            running: AtomicBool::new(false),
        }
    }
}

#[cfg(feature = "sync")]
impl<SPI, D> Bmp390Sync<SpiInterface<SPI>, D> {
    /// Create a new instance of the [`Bmp390Sync`] device.
    ///
    /// # Arguments
    /// * `spi` - The SPI peripheral to use.
    /// * `config` - The configuration to use.
    /// * `delay` - A delay provider to use for initialization.
    ///
    /// # Errors
    /// * Returns [`Bmp390Error::Comm`] if there is an error communicating with the device.
    /// * Returns [`Bmp390Error::InvalidDevice`] if the device ID is incorrect.
    pub async fn new_with_spi(spi: SPI, config: Bmp390Config, delay: D) -> Self {
        Self {
            iface: SpiInterface { spi },
            delay,
            config,
            running: AtomicBool::new(false),
        }
    }
}

/// BMI323 6-axis IMU device
#[cfg(feature = "async")]
pub struct Bmp390Async<IFACE, D, M: RawMutex> {
    /// Interface to communicate with the sensor
    core: Mutex<M, (IFACE, D)>,
    /// Configuration for the sensor
    pub config: Bmp390Config,
    running: AtomicBool,
}

#[cfg(feature = "async")]
impl<I2C, D, M: RawMutex> Bmp390Async<I2cInterface<I2C>, D, M> {
    /// Create a new instance of the [`Bmp390Async`] device.
    ///
    /// # Arguments
    /// * `i2c` - The I2C peripheral to use.
    /// * `address` - The I2C address of the device. Use `DEFAULT_I2C_ADDRESS`.
    /// * `config` - The configuration to use.
    /// * `delay` - A delay provider to use for initialization.
    ///
    /// # Errors
    /// * Returns [`Bmp390Error::Comm`] if there is an error communicating with the device.
    /// * Returns [`Bmp390Error::InvalidDevice`] if the device ID is incorrect.
    pub fn new_with_i2c(i2c: I2C, address: u8, config: Bmp390Config, delay: D) -> Self {
        Self {
            core: Mutex::new((I2cInterface { i2c, address }, delay)),
            config,
            running: AtomicBool::new(false),
        }
    }
}

#[cfg(feature = "async")]
impl<SPI, D, M: RawMutex> Bmp390Async<SpiInterface<SPI>, D, M> {
    /// Create a new instance of the [`Bmp390Async`] device.
    ///
    /// # Arguments
    /// * `spi` - The SPI peripheral to use.
    /// * `config` - The configuration to use.
    /// * `delay` - A delay provider to use for initialization.
    ///
    /// # Errors
    /// * Returns [`Bmp390Error::Comm`] if there is an error communicating with the device.
    /// * Returns [`Bmp390Error::InvalidDevice`] if the device ID is incorrect.
    pub async fn new_with_spi(spi: SPI, config: Bmp390Config, delay: D) -> Self {
        Self {
            core: Mutex::new((SpiInterface { spi }, delay)),
            config,
            running: AtomicBool::new(false),
        }
    }
}

#[cfg(feature = "async")]
impl<IFACE, D, M: RawMutex> Bmp390Async<IFACE, D, M> {
    /// Destroy the device and return the underlying I2C peripheral and delay provider.
    pub fn destroy(self) -> (IFACE, D) {
        let (iface, delay) = self.core.into_inner();
        (iface, delay)
    }
}

/// Errors that can occur when interacting with the MMC5983MA sensor.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Bmp390Error<CommError> {
    /// I2C communication error
    Comm(CommError),
    /// Fatal error reported by the sensor
    FatalError,
    /// Invalid sensor configuration
    InvalidConfiguration,
    /// Invalid device (wrong device ID)
    InvalidDevice,
    /// Driver not ready (e.g., measurement not started)
    NotReady,
    /// No new data available to read
    NoDataAvailable,
    /// Invalid command sent to the sensor
    InvalidCommand,
    /// Invalid access, possible deadlock
    InvalidAccess,
}

impl<CommError> From<CommError> for Bmp390Error<CommError> {
    fn from(err: CommError) -> Self {
        Bmp390Error::Comm(err)
    }
}

/// Sensor readout
#[derive(Debug, Clone, Copy)]
pub struct Measurement {
    /// Raw pressure data
    pub pressure: Option<u32>,
    /// Raw temperature data
    pub temperature: Option<u32>,
}

const MAX_LOOPS: usize = 100;

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
