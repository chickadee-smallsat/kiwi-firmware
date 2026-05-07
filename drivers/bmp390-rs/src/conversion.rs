use crate::trace;

use libm::powf;
use uom::si::f32::{Length, Pressure, ThermodynamicTemperature};
use uom::si::length::foot;
use uom::si::pressure::{hectopascal, pascal};
use uom::si::thermodynamic_temperature::degree_celsius;

use crate::Measurement;

/// Output from the BMP390 consists of ADC outputs.
///
/// These must be compensated using formulas from the datasheet to obtain the actual temperature and pressure values,
/// using coefficients stored in non-volatile memory (NVM).
///
/// # Datasheet
/// - Section 3.11 Output compensation.
/// - Appendix A: Computation formulae reference implementation.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct CalibrationCoefficients {
    par_t1: f32,
    par_t2: f32,
    par_t3: f32,
    par_p1: f32,
    par_p2: f32,
    par_p3: f32,
    par_p4: f32,
    par_p5: f32,
    par_p6: f32,
    par_p7: f32,
    par_p8: f32,
    par_p9: f32,
    par_p10: f32,
    par_p11: f32,
}

impl CalibrationCoefficients {
    /// Calculate the calibration coefficients from the raw register data in registers [`Register::NVM_PAR_T1_0`] to
    /// [`Register::NVM_PAR_P11`].
    ///
    /// # Datasheet
    /// Apendix A, Section 8.4
    pub(crate) fn from_registers(data: &[u8; 21]) -> Self {
        trace!("NVM_PAR: {=[u8]:#04x}", *data);
        let nvm_par_t1: u16 = (data[1] as u16) << 8 | data[0] as u16;
        let nvm_par_t2: u16 = (data[3] as u16) << 8 | data[2] as u16;
        let nvm_par_t3: i8 = data[4] as i8;
        let nvm_par_p1: i16 = (data[6] as i16) << 8 | data[5] as i16;
        let nvm_par_p2: i16 = (data[8] as i16) << 8 | data[7] as i16;
        let nvm_par_p3: i8 = data[9] as i8;
        let nvm_par_p4: i8 = data[10] as i8;
        let nvm_par_p5: u16 = (data[12] as u16) << 8 | data[11] as u16;
        let nvm_par_p6: u16 = (data[14] as u16) << 8 | data[13] as u16;
        let nvm_par_p7: i8 = data[15] as i8;
        let nvm_par_p8: i8 = data[16] as i8;
        let nvm_par_p9: i16 = (data[18] as i16) << 8 | data[17] as i16;
        let nvm_par_p10: i8 = data[19] as i8;
        let nvm_par_p11: i8 = data[20] as i8;

        Self {
            par_t1: (nvm_par_t1 as f32) / 0.003_906_25,    // 2^-8
            par_t2: (nvm_par_t2 as f32) / 1_073_741_824.0, // 2^30
            par_t3: (nvm_par_t3 as f32) / 281_474_976_710_656.0, // 2^48
            par_p1: ((nvm_par_p1 as f32) - 16_384.0) / 1_048_576.0, // 2^14 / 2^20
            par_p2: ((nvm_par_p2 as f32) - 16_384.0) / 536_870_912.0, // 2^14 / 2^29
            par_p3: (nvm_par_p3 as f32) / 4_294_967_296.0, // 2^32
            par_p4: (nvm_par_p4 as f32) / 137_438_953_472.0, // 2^37
            par_p5: (nvm_par_p5 as f32) / 0.125,           // 2^-3
            par_p6: (nvm_par_p6 as f32) / 64.0,            // 2^6
            par_p7: (nvm_par_p7 as f32) / 256.0,           // 2^8
            par_p8: (nvm_par_p8 as f32) / 32768.0,         // 2^15
            par_p9: (nvm_par_p9 as f32) / 281_474_976_710_656.0, //2^48
            par_p10: (nvm_par_p10 as f32) / 281_474_976_710_656.0, // 2^48
            par_p11: (nvm_par_p11 as f32) / 36_893_488_147_419_103_232.0, // 2^65
        }
    }

    /// Obtain a calibrated temperature reading according to calibration coefficients.
    pub fn compensate_temperature(&self, raw_temperature: u32) -> ThermodynamicTemperature {
        // This could be done in fewer expressions, but it's broken down for clarity and to match the datasheet
        let uncompensated = raw_temperature as f32;
        let partial_data1 = uncompensated - self.par_t1;
        let partial_data2 = partial_data1 * self.par_t2;
        let temperature = partial_data2 + (partial_data1 * partial_data1) * self.par_t3;
        ThermodynamicTemperature::new::<degree_celsius>(temperature)
    }

    /// Obtain a calibrated pressure reading according to calibration coefficients and a temperature reading.
    pub fn compensate_pressure(&self, raw_temperature: u32, raw_pressure: u32) -> Pressure {
        // This could be done in fewer expressions, but it's broken down for clarity and to match the datasheet
        let temperature = self
            .compensate_temperature(raw_temperature)
            .get::<degree_celsius>();
        let uncompensated = raw_pressure as f32;
        let partial_data1 = self.par_p6 * temperature;
        let partial_data2 = self.par_p7 * temperature * temperature;
        let partial_data3 = self.par_p8 * temperature * temperature * temperature;
        let partial_out1 = self.par_p5 + partial_data1 + partial_data2 + partial_data3;

        let partial_data1 = self.par_p2 * temperature;
        let partial_data2 = self.par_p3 * temperature * temperature;
        let partial_data3 = self.par_p4 * temperature * temperature * temperature;
        let partial_out2 =
            uncompensated * (self.par_p1 + partial_data1 + partial_data2 + partial_data3);

        let partial_data1 = uncompensated * uncompensated;
        let partial_data2 = self.par_p9 + self.par_p10 * temperature;
        let partial_data3 = partial_data1 * partial_data2;
        let partial_data4 =
            partial_data3 + uncompensated * uncompensated * uncompensated * self.par_p11;

        let pressure = partial_out1 + partial_out2 + partial_data4;
        Pressure::new::<pascal>(pressure)
    }

    /// Convert raw measurement data into calibrated temperature, pressure, and altitude values.
    pub fn convert(
        &self,
        meas: Measurement,
        altitude_reference: Length,
    ) -> Option<(ThermodynamicTemperature, Pressure, Length)> {
        match (meas.temperature, meas.pressure) {
            (Some(raw_temp), Some(raw_pres)) => {
                let temp = self.compensate_temperature(raw_temp);
                let pres = self.compensate_pressure(raw_temp, raw_pres);
                Some((temp, pres, calculate_altitude(pres, altitude_reference)))
            }
            _ => None,
        }
    }
}

/// Calculate the altitude based on the pressure, sea level pressure, and the reference altitude.
///
/// The altitude is calculating following the [NOAA formula](https://www.weather.gov/media/epz/wxcalc/pressureAltitude.pdf).
fn calculate_altitude(pressure: Pressure, altitude_reference: Length) -> Length {
    let sea_level = Pressure::new::<hectopascal>(1013.25);
    let above_sea_level =
        Length::new::<foot>(145366.45 * (1.0 - powf((pressure / sea_level).value, 0.190284)));

    above_sea_level - altitude_reference
}
