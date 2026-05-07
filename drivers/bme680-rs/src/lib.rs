#![no_std]
#![deny(missing_docs)]

//! Async driver for the Bosch BME680 environmental sensor, supporting temperature,
//! humidity, pressure, and gas resistance measurements over I²C.
//! The driver is designed for embedded environments and uses the `embedded-hal-async` traits
//! for hardware abstraction, allowing it to be used with a variety of microcontrollers and platforms.
//! The driver handles sensor initialization, configuration, and measurement in a non-blocking manner,
//! making it suitable for use in async applications where efficient use of resources is important.

#[cfg(feature = "defmt-messages")]
#[allow(unused_imports)]
use defmt::{debug, error, info, trace, warn};

// No-op logging macros when defmt is not enabled.
#[cfg(not(feature = "defmt-messages"))]
mod log_impl {
    #![allow(unused_macros, unused_imports)]
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

mod interface;
mod registers;

pub use registers::Oversampling;
pub use uom::si::{
    electrical_resistance::ohm,
    f32::{ElectricalResistance, Pressure, Ratio, ThermodynamicTemperature},
    pressure::pascal,
    ratio::percent,
    thermodynamic_temperature::degree_celsius,
};

use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::i2c::I2c;
use interface::{I2cInterface, Interface as _};
use uom::si::thermodynamic_temperature::degree_celsius as _degree_celsius;

/// I²C address of the BME680.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bme680Address {
    /// Address pin tied low → 0x76
    AddrLow = 0x76,
    /// Address pin tied high → 0x77
    AddrHigh = 0x77,
}

/// Driver errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error<E> {
    /// Underlying I²C bus error.
    I2c(E),
    /// CHIP_ID register did not return 0x61.
    InvalidChipId,
    /// Forced-mode measurement timed out (no `new_data` flag after many polls).
    MeasurementTimedOut,
}

impl<E> From<E> for Error<E> {
    fn from(e: E) -> Self {
        Self::I2c(e)
    }
}

/// Sensor configuration passed to [`Bme680::init`].
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Temperature oversampling.
    pub os_temperature: Oversampling,
    /// Pressure oversampling.
    pub os_pressure: Oversampling,
    /// Humidity oversampling.
    pub os_humidity: Oversampling,
    /// Gas heater target temperature in degrees Celsius (capped at 400 °C).
    pub heater_temperature: u16,
    /// Gas heater on-time in milliseconds.
    pub heater_duration_ms: u16,
    /// Approximate ambient temperature in degrees Celsius, used to calculate
    /// the heater resistance register value.
    pub ambient_temperature: i8,
}

impl Config {
    /// Const-fn default
    pub const fn default() -> Self {
        Self {
            os_temperature: Oversampling::X2,
            os_pressure: Oversampling::X16,
            os_humidity: Oversampling::X1,
            heater_temperature: 320,
            heater_duration_ms: 100,
            ambient_temperature: 25,
        }
    }

    /// Calculate the worst-case measurement duration in milliseconds for a given
    /// oversampling and heater configuration.
    ///
    /// Formula from BME680 datasheet §3.2.1:
    /// `t = 1.25 + 2.3·T + (2.3·P + 0.575) + (2.3·H + 0.575) + t_gas` (ms)
    pub const fn measure_duration_ms(&self) -> u32 {
        // All durations expressed in units of 1/100 ms to stay in integer arithmetic.
        // Formula from BME680 datasheet §3.2.1:
        //   t = 1.25 + 2.3·T + (2.3·P + 0.575) + (2.3·H + 0.575) + t_gas  [ms]
        let none = Oversampling::None as u8;
        let os_p_val = self.os_pressure as u8;
        let os_h_val = self.os_humidity as u8;
        let base: u32 = 125; // 1.25 ms × 100
        let t_temp: u32 = 230 * self.os_temperature.samples(); // 2.3 ms × 100 per sample
        let t_pres: u32 = if os_p_val != none {
            230 * self.os_pressure.samples() + 58 // 0.575 ms × 100 ≈ 58
        } else {
            0
        };
        let t_hum: u32 = if os_h_val != none {
            230 * self.os_humidity.samples() + 58
        } else {
            0
        };
        let t_gas: u32 = self.heater_duration_ms as u32 * 100;
        let total_hundredths = base + t_temp + t_pres + t_hum + t_gas;
        // Ceiling division by 100 to get whole milliseconds.
        total_hundredths.div_ceil(100)
    }
}

/// Compensated measurement returned by [`Bme680::measure`].
#[derive(Clone, Copy, Debug)]
pub struct Measurement {
    /// Compensated temperature.
    pub temperature: ThermodynamicTemperature,
    /// Compensated pressure.
    pub pressure: Pressure,
    /// Compensated relative humidity (0–100 %).
    pub humidity: Ratio,
    /// Compensated gas resistance in Ohms.
    pub gas_resistance: ElectricalResistance,
    /// `true` when both the heater was stable and the gas reading is marked valid.
    pub gas_valid: bool,
}

/// BME680 driver.
pub struct Bme680<I2C, D> {
    iface: I2cInterface<I2C>,
    delay: D,
    calib: registers::Calibration,
    /// Worst-case measurement duration in ms, derived from the last [`init`](Self::init) config.
    meas_duration_ms: u32,
}

impl<I2C, E, D> Bme680<I2C, D>
where
    I2C: I2c<Error = E>,
    D: DelayNs,
{
    /// Create a new driver instance.
    pub fn new(i2c: I2C, addr: Bme680Address, delay: D) -> Self {
        let default_cfg = Config::default();
        Self {
            iface: I2cInterface {
                i2c,
                address: addr as u8,
            },
            delay,
            calib: registers::Calibration::default(),
            meas_duration_ms: default_cfg.measure_duration_ms(),
        }
    }

    /// Initialise the sensor: soft-reset, verify chip-ID, read calibration,
    /// and apply `config` (oversampling + gas heater settings).
    ///
    /// Must be called once before [`measure`](Self::measure).
    /// Can be called again to reconfigure without reconstructing the driver.
    pub async fn init(&mut self, config: Config) -> Result<(), Error<E>> {
        debug!("BME680: reset");
        registers::Reset::reset(&mut self.iface).await?;
        self.delay.delay_ms(10).await;

        debug!("BME680: verifying chip ID");
        registers::ChipId::read(&mut self.iface).await?;

        debug!("BME680: reading calibration");
        self.calib = registers::Calibration::read(&mut self.iface).await?;

        // Compute and cache worst-case measurement duration for this config.
        self.meas_duration_ms = config.measure_duration_ms();

        // Write oversampling settings in sleep mode.
        let ctrl = registers::CtrlTempPres::new()
            .with_mode(registers::MeasurementMode::Sleep)
            .with_pressure(config.os_pressure)
            .with_temperature(config.os_temperature);
        self.iface
            .write_registers(registers::CTRL_MEAS, &[u8::from(ctrl)])
            .await?;

        let ctrl_hum = registers::CtrlHumGas::new().with_humidity(config.os_humidity);
        self.iface
            .write_registers(registers::CTRL_HUM, &[u8::from(ctrl_hum)])
            .await?;

        // Gas heater profile 0.
        let res_heat = registers::calc_res_heat(
            config.heater_temperature,
            config.ambient_temperature,
            &self.calib.gas,
        );
        let gas_wait = registers::calc_gas_wait(config.heater_duration_ms);
        self.iface
            .write_registers(registers::RES_HEAT_0, &[res_heat])
            .await?;
        self.iface
            .write_registers(registers::GAS_WAIT_0, &[gas_wait])
            .await?;

        let ctrl_gas0 = registers::CtrlGas0::new().with_heat_off(false);
        self.iface
            .write_registers(registers::CTRL_GAS_0, &[u8::from(ctrl_gas0)])
            .await?;

        let ctrl_gas1 = registers::CtrlGas1::new().with_run_gas(true).with_index(0);
        self.iface
            .write_registers(registers::CTRL_GAS_1, &[u8::from(ctrl_gas1)])
            .await?;

        debug!(
            "BME680: init complete, meas_duration={} ms",
            self.meas_duration_ms
        );
        Ok(())
    }

    /// Trigger a single forced-mode measurement and return compensated data.
    ///
    /// Polls `new_data` at 10 ms intervals for up to ~300 ms.
    /// Returns [`Error::MeasurementTimedOut`] if the sensor does not respond.
    pub async fn measure(&mut self) -> Result<Measurement, Error<E>> {
        // Ensure sleep mode before triggering.
        let mut ctrl_buf = [0u8; 1];
        self.iface
            .read_registers(registers::CTRL_MEAS, &mut ctrl_buf)
            .await?;
        let mut ctrl = registers::CtrlTempPres::from(ctrl_buf[0]);
        if ctrl.mode() != registers::MeasurementMode::Sleep {
            ctrl = ctrl.with_mode(registers::MeasurementMode::Sleep);
            self.iface
                .write_registers(registers::CTRL_MEAS, &[u8::from(ctrl)])
                .await?;
            self.delay.delay_ms(10).await;
        }

        // Trigger forced mode.
        trace!("BME680: triggering forced-mode measurement");
        self.iface
            .write_registers(
                registers::CTRL_MEAS,
                &[u8::from(ctrl.with_mode(registers::MeasurementMode::Forced))],
            )
            .await?;

        // After the expected measurement duration, read the full 17-byte field
        // and check new_data.  Reading the field in one burst (as the Bosch
        // reference API does) avoids a race where a separate status-only read
        // clears the new_data flag before the data read.
        self.delay.delay_ms(self.meas_duration_ms).await;
        let mut buf = [0u8; 17];
        let mut data_ready = false;
        for _ in 0..5u8 {
            self.iface
                .read_registers(registers::MEAS_STATUS_0, &mut buf)
                .await?;
            if registers::Status::from(buf[0]).new_data() {
                data_ready = true;
                break;
            }
            self.delay.delay_ms(5).await;
        }

        if !data_ready {
            warn!(
                "BME680: measurement timed out after {} ms",
                self.meas_duration_ms + 5 * 5
            );
            return Err(Error::MeasurementTimedOut);
        }

        // Parse raw ADC values.
        // Press   bytes [2-4]  (20-bit): buf[2]<<12 | buf[3]<<4 | buf[4]>>4
        // Temp    bytes [5-7]  (20-bit): buf[5]<<12 | buf[6]<<4 | buf[7]>>4
        // Humidity bytes [8-9] (16-bit)
        // Gas ADC bytes [13-14] (10-bit): buf[13]<<2 | buf[14]>>6
        let adc_pres = (buf[2] as u32) << 12 | (buf[3] as u32) << 4 | (buf[4] as u32) >> 4;
        let adc_temp = (buf[5] as u32) << 12 | (buf[6] as u32) << 4 | (buf[7] as u32) >> 4;
        let adc_hum = (buf[8] as u16) << 8 | buf[9] as u16;
        let adc_gas = (buf[13] as u16) << 2 | (buf[14] >> 6) as u16;
        let gas_range = buf[14] & 0x0F;
        let heat_stab = (buf[14] & 0x10) != 0; // bit 4
        let gasm_valid = (buf[14] & 0x20) != 0; // bit 5

        trace!(
            "BME680: adc_temp={} adc_pres={} adc_hum={} adc_gas={} gas_range={} heat_stab={} gasm_valid={}",
            adc_temp, adc_pres, adc_hum, adc_gas, gas_range, heat_stab, gasm_valid
        );

        let mut t_fine = 0.0_f32;
        let temperature =
            registers::compensate_temperature(adc_temp, &mut t_fine, &self.calib.temp);
        let pressure = registers::compensate_pressure(adc_pres, t_fine, &self.calib.press);
        let humidity = registers::compensate_humidity(adc_hum, t_fine, &self.calib.hum);
        let gas_resistance =
            registers::compensate_gas_resistance_low(adc_gas, gas_range, &self.calib.gas);

        Ok(Measurement {
            temperature: ThermodynamicTemperature::new::<_degree_celsius>(temperature),
            pressure: Pressure::new::<pascal>(pressure),
            humidity: Ratio::new::<percent>(humidity),
            gas_resistance: ElectricalResistance::new::<ohm>(gas_resistance),
            gas_valid: heat_stab && gasm_valid,
        })
    }
}
