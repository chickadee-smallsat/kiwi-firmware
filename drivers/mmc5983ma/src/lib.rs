#![no_std]
#![deny(missing_docs)]
//! `no_std` driver for the MMC5983MA 3-axis magnetometer

use core::{ops::Div, sync::atomic::AtomicBool};

use bitfield_struct::bitfield;
#[cfg(feature = "async")]
use embassy_sync::{blocking_mutex::raw::RawMutex, mutex::Mutex};
use uom::ConstZero;

use crate::{
    config::Mmc5983Config,
    interface::{I2cInterface, SpiInterface},
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
#[cfg(feature = "float")]
mod float;
mod interface;
mod registers;
#[cfg(feature = "sync")]
mod sync;

/// Default I2C address for the MMC5983MA sensor
pub const DEFAULT_I2C_ADDRESS: u8 = 0x30;

pub use crate::config::{
    AxisInhibit, ContinuousMeasurementFreq, DecimationBw, Mmc5983ConfigBuilder, PeriodicSetInterval,
};
#[cfg(feature = "async")]
pub use crate::r#async::AsyncInterface;
#[cfg(feature = "sync")]
pub use crate::sync::SyncInterface;

#[cfg(feature = "sync")]
/// MMC5983MA 3-axis magnetometer device
pub struct Mmc5983Sync<IFACE, D> {
    iface: IFACE,
    delay: D,
    config: Mmc5983Config,
    ofst: Option<MagMeasurementRaw>,
    running: AtomicBool,
}

#[cfg(feature = "sync")]
impl<I2C, D> Mmc5983Sync<I2cInterface<I2C>, D> {
    /// Create a new instance of the [`Mmc5983Sync`] device.
    ///
    /// # Arguments
    /// * `i2c` - The I2C peripheral to use.
    /// * `address` - The I2C address of the device. Use `DEFAULT_I2C_ADDRESS`.
    /// * `config` - The configuration to use.
    /// * `delay` - A delay provider to use for initialization.
    pub fn new_with_i2c(i2c: I2C, address: u8, config: Mmc5983Config, delay: D) -> Self {
        Self {
            iface: I2cInterface { i2c, address },
            delay,
            config,
            ofst: None,
            running: AtomicBool::new(false),
        }
    }
}

#[cfg(feature = "sync")]
impl<SPI, D> Mmc5983Sync<SpiInterface<SPI>, D> {
    /// Create a new instance of the [`Mmc5983Sync`] device.
    ///
    /// # Arguments
    /// * `spi` - The SPI peripheral to use.
    /// * `config` - The configuration to use.
    /// * `delay` - A delay provider to use for initialization.
    pub fn new_with_spi(spi: SPI, config: Mmc5983Config, delay: D) -> Self {
        Self {
            iface: SpiInterface { spi },
            delay,
            config,
            ofst: None,
            running: AtomicBool::new(false),
        }
    }
}

#[cfg(feature = "async")]
/// MMC5983MA 3-axis magnetometer device
pub struct Mmc5983Async<IFACE, D, M: RawMutex> {
    core: Mutex<M, (IFACE, D)>,
    config: Mmc5983Config,
    ofst: Option<MagMeasurementRaw>,
    running: AtomicBool,
}

#[cfg(feature = "async")]
impl<I2C, D, M: RawMutex> Mmc5983Async<I2cInterface<I2C>, D, M> {
    /// Create a new instance of the [`Mmc5983Async`] device.
    ///
    /// # Arguments
    /// * `i2c` - The I2C peripheral to use.
    /// * `address` - The I2C address of the device. Use `DEFAULT_I2C_ADDRESS`.
    /// * `config` - The configuration to use.
    /// * `delay` - A delay provider to use for initialization.
    pub fn new_with_i2c(i2c: I2C, address: u8, config: Mmc5983Config, delay: D) -> Self {
        Self {
            core: Mutex::new((I2cInterface { i2c, address }, delay)),
            config,
            ofst: None,
            running: AtomicBool::new(false),
        }
    }
}

#[cfg(feature = "async")]
impl<SPI, D, M: RawMutex> Mmc5983Async<SpiInterface<SPI>, D, M> {
    /// Create a new instance of the [`Mmc5983Async`] device.
    ///
    /// # Arguments
    /// * `spi` - The SPI peripheral to use.
    /// * `config` - The configuration to use.
    /// * `delay` - A delay provider to use for initialization.
    pub fn new_with_spi(spi: SPI, config: Mmc5983Config, delay: D) -> Self {
        Self {
            core: Mutex::new((SpiInterface { spi }, delay)),
            config,
            ofst: None,
            running: AtomicBool::new(false),
        }
    }
}

/// Errors that can occur when interacting with the MMC5983MA sensor.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Mmc5983Error<CommError> {
    /// I2C communication error
    Comm(CommError),
    /// Invalid device ID
    InvalidDevice,
    /// Measurement not ready
    NotReady,
    /// Invalid configuration
    InvalidConfig,
    /// Invalid access
    InvalidAccess,
}

impl<CommError> From<CommError> for Mmc5983Error<CommError> {
    fn from(err: CommError) -> Self {
        Mmc5983Error::Comm(err)
    }
}

/// Raw measurement data from the sensor
#[bitfield(u64)]
pub struct MagMeasurementRaw {
    #[bits(18)]
    pub x: u32,
    #[bits(18)]
    pub y: u32,
    #[bits(18)]
    pub z: u32,
    #[bits(10)]
    _reserved: u32,
}

#[cfg(feature = "defmt")]
impl defmt::Format for MagMeasurementRaw {
    fn format(&self, fmt: defmt::Formatter) {
        defmt::write!(
            fmt,
            "MagMeasurementRaw {{ x: {}, y: {}, z: {} }}",
            self.x(),
            self.y(),
            self.z()
        );
    }
}

impl ConstZero for MagMeasurementRaw {
    const ZERO: Self = Self(0);
}

impl Div<u32> for MagMeasurementRaw {
    type Output = Self;

    fn div(self, rhs: u32) -> Self::Output {
        Self::new()
            .with_x(self.x() / rhs)
            .with_y(self.y() / rhs)
            .with_z(self.z() / rhs)
    }
}

/// Raw temperature measurement from the sensor
pub struct TempMeasurementRaw(pub u8);

const MAX_LOOPS: usize = 100;

#[cfg(feature = "float")]
pub use float::MagMeasurement;

#[cfg(not(feature = "float"))]
pub use non_float::MagMeasurement;
#[cfg(not(feature = "float"))]
mod non_float {
    use core::ops::Sub;

    use crate::MagMeasurementRaw;
    /// Converted measurement data from the sensor
    pub struct MagMeasurement {
        /// X axis measurement
        pub x: i32,
        /// Y axis measurement
        pub y: i32,
        /// Z axis measurement
        pub z: i32,
        /// Scale factor to convert raw measurement to Gauss units: 1 LSB = 1 Gauss / scale
        pub scale: i32,
    }

    impl Default for MagMeasurement {
        fn default() -> Self {
            Self {
                x: 1 << 17,
                y: 1 << 17,
                z: 1 << 17,
                scale: 16384,
            }
        }
    }

    #[cfg(feature = "defmt")]
    impl defmt::Format for MagMeasurement {
        fn format(&self, fmt: defmt::Formatter) {
            defmt::write!(
                fmt,
                "MagMeasurement {{ x: {}, y: {}, z: {}, scale: {} }}",
                self.x,
                self.y,
                self.z,
                self.scale
            );
        }
    }

    impl Sub for MagMeasurement {
        type Output = Self;

        fn sub(self, rhs: Self) -> Self::Output {
            Self {
                x: self.x - rhs.x,
                y: self.y - rhs.y,
                z: self.z - rhs.z,
                scale: self.scale,
            }
        }
    }

    impl From<MagMeasurementRaw> for MagMeasurement {
        fn from(value: MagMeasurementRaw) -> Self {
            Self {
                x: value.x() as i32,
                y: value.y() as i32,
                z: value.z() as i32,
                scale: 16384,
            }
        }
    }

    impl From<Option<MagMeasurementRaw>> for MagMeasurement {
        fn from(value: Option<MagMeasurementRaw>) -> Self {
            match value {
                Some(raw) => Self::from(raw),
                None => Self::default(),
            }
        }
    }
}
