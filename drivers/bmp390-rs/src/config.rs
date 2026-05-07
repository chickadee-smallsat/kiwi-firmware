use crate::{
    registers::{IrqControl, OversamplingReg, PowerCtrl},
    IIRFilterConfig, OutputDataRate, Oversampling, SensorMode,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Configuration for an interrupt pin.
pub struct IrqPinConfig {
    /// true if the interrupt pin is active high, false if active low.
    pub active_high: bool,
    /// true if the interrupt pin is push-pull, false if open-drain.
    pub open_drain: bool,
}

#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Configuration for the BMI323 sensor.
pub struct Bmp390Config {
    pub(crate) irq_control: IrqControl,
    pub(crate) sensor_mode: SensorMode,
    pub(crate) pres_ovsamp: Oversampling,
    pub(crate) temp_ovsamp: Oversampling,
    pub(crate) odr: OutputDataRate,
    pub(crate) filtercfg: IIRFilterConfig,
    pub(crate) min_delay_us: u32,
}

impl Default for Bmp390Config {
    /// Enables the temperature and pressure sensors in high performance mode.
    /// - Pressure oversampling: x16
    /// - Temperature oversampling: x2
    /// - Output data rate: 25 Hz
    /// - IIR filter: Bypassed
    /// - Interrupt pin: Disabled, active low, push-pull
    fn default() -> Self {
        Self {
            pres_ovsamp: Oversampling::X16,
            temp_ovsamp: Oversampling::X2,
            sensor_mode: SensorMode::default(),
            irq_control: IrqControl::default(),
            odr: OutputDataRate::default(),
            filtercfg: IIRFilterConfig::default(),
            min_delay_us: 40000,
        }
    }
}

impl Bmp390Config {
    /// Set the sensor mode.
    pub fn with_sensor_mode(mut self, mode: SensorMode) -> Self {
        self.sensor_mode = mode;
        self
    }
    /// Set the pressure oversampling.
    pub fn with_pressure_oversampling(mut self, os: Oversampling) -> Self {
        self.pres_ovsamp = os;
        self
    }
    /// Set the temperature oversampling.
    pub fn with_temperature_oversampling(mut self, os: Oversampling) -> Self {
        self.temp_ovsamp = os;
        self
    }
    /// Set the output data rate.
    pub fn with_output_data_rate(mut self, odr: OutputDataRate) -> Self {
        self.odr = odr;
        self.build();
        self
    }
    /// Set the IIR filter configuration.
    pub fn with_iir_filter_config(mut self, cfg: IIRFilterConfig) -> Self {
        self.filtercfg = cfg;
        self
    }

    /// Set the interrupt pin configuration.
    pub fn with_irq_pin_config(mut self, cfg: IrqPinConfig) -> Self {
        self.irq_control.set_open_drain(cfg.open_drain);
        self.irq_control.set_active_high(cfg.active_high);
        self
    }

    /// Enable or disable the interrupt pin.
    pub fn enable_irq(mut self, enable: bool) -> Self {
        self.irq_control.set_drdy(enable);
        self
    }

    /// Get the current sensor mode.
    pub fn sensor_mode(&self) -> SensorMode {
        self.sensor_mode
    }

    /// Get the current pressure oversampling.
    pub fn pressure_oversampling(&self) -> Oversampling {
        self.pres_ovsamp
    }

    /// Get the current temperature oversampling.
    pub fn temperature_oversampling(&self) -> Oversampling {
        self.temp_ovsamp
    }

    /// Get the current output data rate.
    pub fn output_data_rate(&self) -> OutputDataRate {
        self.odr
    }

    /// Get the current IIR filter configuration.
    pub fn iir_filter_config(&self) -> IIRFilterConfig {
        self.filtercfg
    }

    /// Get the current interrupt pin configuration.
    pub fn irq_pin_config(&self) -> IrqPinConfig {
        IrqPinConfig {
            active_high: self.irq_control.active_high(),
            open_drain: self.irq_control.open_drain(),
        }
    }

    /// Check if the interrupt pin is enabled.
    pub fn irq_enabled(&self) -> bool {
        self.irq_control.drdy()
    }

    fn build(&mut self) {
        self.min_delay_us = self.odr.delay();
    }

    #[must_use]
    pub(crate) fn oversamp_config(&self) -> OversamplingReg {
        OversamplingReg::new()
            .with_pressure(self.pres_ovsamp)
            .with_temperature(self.temp_ovsamp)
    }

    #[must_use]
    pub(crate) fn power_ctrl(&self) -> PowerCtrl {
        PowerCtrl::new()
            .with_mode(self.sensor_mode)
            .with_press_en(true)
            .with_temp_en(true)
    }
}
