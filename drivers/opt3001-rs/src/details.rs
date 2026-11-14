use crate::{Config, ConversionTime, Mode};

pub(crate) const REG_RESULT: u8 = 0x00;
pub(crate) const REG_CONFIGURATION: u8 = 0x01;
pub(crate) const REG_MANUFACTURER_ID: u8 = 0x7E;
pub(crate) const REG_DEVICE_ID: u8 = 0x7F;

pub(crate) const MANUFACTURER_ID: u16 = 0x5449;
pub(crate) const DEVICE_ID: u16 = 0x3001;

/// Measured ambient light value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct MeasurementRaw {
    /// Illuminance in milli-lux.
    pub illuminance_mlux: u32,
}

impl TryFrom<u16> for MeasurementRaw {
    type Error = &'static str;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        let exponent = (value >> 12) as u8;
        if exponent > 11 {
            return Err("invalid result exponent");
        }
        let mantissa = value & 0x0fff;
        Ok(Self {
            illuminance_mlux: mantissa as u32 * 10 * (1_u32 << exponent),
        })
    }
}

/// Status bits read from the OPT3001 configuration register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Status {
    /// The result register has overflowed.
    pub overflow: bool,
    /// A conversion has completed.
    pub conversion_ready: bool,
    /// The high-limit comparison flag is set.
    pub high_limit: bool,
    /// The low-limit comparison flag is set.
    pub low_limit: bool,
    /// The conversion mode currently reported by the sensor.
    pub mode: Mode,
}

impl From<u16> for Status {
    fn from(value: u16) -> Self {
        Self {
            overflow: value & (1 << 8) != 0,
            conversion_ready: value & (1 << 7) != 0,
            high_limit: value & (1 << 6) != 0,
            low_limit: value & (1 << 5) != 0,
            mode: Mode::from_bits(((value >> 9) & 0b11) as u8),
        }
    }
}

impl Config {
    #[inline]
    pub(crate) const fn bits(self) -> u16 {
        ((self.range as u16) << 12)
            | ((self.conversion_time as u16) << 11)
            | ((self.mode as u16) << 9)
            | ((self.latch as u16) << 4)
            | ((self.interrupt_polarity as u16) << 3)
            | ((self.mask_exponent as u16) << 2)
            | self.fault_count as u16
    }

    #[inline]
    pub(crate) const fn with_mode(self, mode: Mode) -> Self {
        Self { mode, ..self }
    }
}

impl ConversionTime {
    #[inline]
    pub(crate) const fn conversion_time_ms(self) -> u32 {
        match self {
            Self::Ms100 => 100,
            Self::Ms800 => 800,
        }
    }
}

impl Mode {
    #[inline]
    pub(crate) const fn from_bits(value: u8) -> Self {
        match value {
            0b01 => Self::SingleShot,
            0b10 | 0b11 => Self::Continuous,
            _ => Self::Shutdown,
        }
    }
}

#[cfg(feature = "float")]
pub use uom::si::f32::Illuminance;

#[cfg(feature = "float")]
/// Measured ambient light value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeasurementUnit {
    /// Illuminance.
    pub illuminance: Illuminance,
}

#[cfg(feature = "float")]
impl From<MeasurementRaw> for MeasurementUnit {
    fn from(raw: MeasurementRaw) -> Self {
        use uom::si::illuminance::lux;

        Self {
            illuminance: Illuminance::new::<lux>(raw.illuminance_mlux as f32 / 1000.0),
        }
    }
}
