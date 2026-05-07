use crate::error;

use crate::Oversampling;
/// Factory calibration of the device
#[derive(Debug, Clone)]
pub(crate) struct Calibration {
    /// Pressure sensitivity at reference temperature
    pub(crate) sens_tref: u16,
    /// Pressure offset at reference temperature
    pub(crate) ofst_tref: u16,
    /// Temperature coefficient of pressure sensitivity
    pub(crate) tcs_p: u16,
    /// Temperature coefficient of pressure offset
    pub(crate) tco_p: u16,
    /// Reference temperature
    pub(crate) tref: u16,
    /// Temperature coefficient of the temperature
    pub(crate) tcs_t: u16,
}

impl TryFrom<[u8; 16]> for Calibration {
    type Error = &'static str;

    fn try_from(value: [u8; 16]) -> Result<Self, Self::Error> {
        let mut value = value;
        // CRC calculation
        let crc_in = value[15] & 0xf; // Get the CRC value
        let mut crc_out = 0; // Initialize the calculated CRC value
        value[15] = 0; // Reset the CRC value to 0
        for byte in value.iter().copied() {
            crc_out ^= byte as u32; // XOR the byte with the calculated CRC value
            for _ in 0..8 {
                if crc_out & 0x8000 != 0 {
                    crc_out = (crc_out << 1) ^ 0x3000; // Shift left and XOR with polynomial
                } else {
                    crc_out <<= 1; // Just shift left
                }
            }
        }
        crc_out >>= 12; // Shift right to get the final CRC value
        if (crc_out & 0xf) as u8 != crc_in {
            error!(
                "CRC check failed: expected {}, got {}",
                crc_in, crc_out as u8
            );
            return Err("CRC check failed");
        }

        Ok(Self {
            sens_tref: u16::from_be_bytes([value[2], value[3]]),
            ofst_tref: u16::from_be_bytes([value[4], value[5]]),
            tcs_p: u16::from_be_bytes([value[6], value[7]]),
            tco_p: u16::from_be_bytes([value[8], value[9]]),
            tref: u16::from_be_bytes([value[10], value[11]]),
            tcs_t: u16::from_be_bytes([value[12], value[13]]),
        })
    }
}

impl Calibration {
    /// Compensate the raw pressure and temperature readings using the factory calibration data
    pub(crate) fn compensate(&self, pres: u32, temp: u32) -> MeasurementRaw {
        let d_t = temp as i32 - (self.tref as i32 * (1 << 8));
        let temp = 2000 + (d_t * self.tcs_t as i32) / (1 << 23);
        let off =
            (self.ofst_tref as i64 * (1 << 17)) + ((self.tco_p as i64 * d_t as i64) / (1 << 6));
        let sens =
            (self.sens_tref as i64 * (1 << 16)) + ((self.tcs_p as i64 * d_t as i64) / (1 << 7));
        // Second order compensation is only needed for low temperatures, so we can skip it if the temperature is above 20°C
        if temp >= 2000 {
            let press = (((pres as i64 * sens) / (1 << 21)) - off) / (1 << 15);
            return MeasurementRaw {
                temperature: temp,
                pressure: press as i32,
            };
        }
        // Second order compensation
        let t_2000 = temp as i64 - 2000;
        let dt2 = (d_t as i64 * d_t as i64) / (1 << 31);
        let mut off2 = 61 * t_2000 * t_2000 / (1 << 4);
        let mut sens2 = 2 * t_2000 * t_2000;
        if temp < -1500 {
            let t_1500 = temp as i64 + 1500;
            off2 += 15 * t_1500 * t_1500;
            sens2 += 8 * t_1500 * t_1500;
        }
        let temp = temp as i64 - dt2;
        let off = off - off2;
        let sens = sens - sens2;
        let press = (((pres as i64 * sens) / (1 << 21)) - off) / (1 << 15);
        MeasurementRaw {
            temperature: temp as i32,
            pressure: press as i32,
        }
    }
}

impl Oversampling {
    #[inline(always)]
    pub(crate) const fn conversion_time_us(self) -> u32 {
        match self {
            Oversampling::Osr256 => 600,
            Oversampling::Osr512 => 1170,
            Oversampling::Osr1024 => 2280,
            Oversampling::Osr2048 => 4540,
            Oversampling::Osr4096 => 9040,
        }
    }
}

pub(crate) const CMD_RESET: u8 = 0x1E;
pub(crate) const CMD_CONV_D1: u8 = 0x40;
pub(crate) const CMD_CONV_D2: u8 = 0x50;
pub(crate) const CMD_ADC_READ: u8 = 0x00;
pub(crate) const CMD_PROM_READ: u8 = 0xA0;

/// Measured pressure and temperature values
pub struct MeasurementRaw {
    /// Compensated temperature in hundredths of a degree Celsius
    pub temperature: i32,
    /// Compensated pressure in hundredths of a millibar
    pub pressure: i32,
}

#[cfg(feature = "float")]
pub use uom::si::f32::{Pressure, ThermodynamicTemperature};

#[cfg(feature = "float")]
/// Measured pressure and temperature values
pub struct MeasurementUnit {
    /// Compensated temperature
    pub temperature: ThermodynamicTemperature,
    /// Compensated pressure
    pub pressure: Pressure,
}

#[cfg(feature = "float")]
impl From<MeasurementRaw> for MeasurementUnit {
    fn from(raw: MeasurementRaw) -> Self {
        use uom::si::{pressure::millibar, thermodynamic_temperature::degree_celsius};

        Self {
            temperature: ThermodynamicTemperature::new::<degree_celsius>(
                raw.temperature as f32 / 100.0,
            ),
            pressure: Pressure::new::<millibar>(raw.pressure as f32 / 100.0),
        }
    }
}
