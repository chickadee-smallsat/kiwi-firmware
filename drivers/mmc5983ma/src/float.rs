use core::ops::Sub;

use uom::si::{
    f32::{MagneticFluxDensity, ThermodynamicTemperature},
    magnetic_flux_density::gauss,
    thermodynamic_temperature::degree_celsius,
};

use crate::{MagMeasurementRaw, TempMeasurementRaw};

/// Converted measurement data from the sensor
pub struct MagMeasurement {
    /// X axis measurement in milliGauss
    pub x: MagneticFluxDensity,
    /// Y axis measurement in milliGauss
    pub y: MagneticFluxDensity,
    /// Z axis measurement in milliGauss
    pub z: MagneticFluxDensity,
}

impl Default for MagMeasurement {
    fn default() -> Self {
        Self {
            x: convert_u32(1 << 17),
            y: convert_u32(1 << 17),
            z: convert_u32(1 << 17),
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

#[cfg(feature = "defmt")]
impl defmt::Format for MagMeasurement {
    fn format(&self, fmt: defmt::Formatter) {
        defmt::write!(
            fmt,
            "{{ x: {} mG, y: {} mG, z: {} mG }}",
            self.x.get::<gauss>() * 1e3,
            self.y.get::<gauss>() * 1e3,
            self.z.get::<gauss>() * 1e3
        );
    }
}

impl From<MagMeasurementRaw> for MagMeasurement {
    fn from(value: MagMeasurementRaw) -> Self {
        Self {
            x: convert_u32(value.x()),
            y: convert_u32(value.y()),
            z: convert_u32(value.z()),
        }
    }
}

impl MagMeasurement {
    /// Convert the measurement to milliGauss
    pub fn milligauss(&self) -> (f32, f32, f32) {
        (
            self.x.get::<gauss>() * 1e3,
            self.y.get::<gauss>() * 1e3,
            self.z.get::<gauss>() * 1e3,
        )
    }
}

#[inline(always)]
fn convert_u32(x: u32) -> MagneticFluxDensity {
    MagneticFluxDensity::new::<gauss>(x as f32 / 16384.0)
}

impl From<TempMeasurementRaw> for ThermodynamicTemperature {
    fn from(value: TempMeasurementRaw) -> Self {
        ThermodynamicTemperature::new::<degree_celsius>(value.0 as f32 * 0.8 - 75.0)
    }
}

impl Sub for MagMeasurement {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self {
            x: self.x - rhs.x,
            y: self.y - rhs.y,
            z: self.z - rhs.z,
        }
    }
}
