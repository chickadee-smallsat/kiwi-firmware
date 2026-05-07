use crate::registers::{AnalogControl, DigitalControl, MeasurementTriggerControl};
/// Bandwidth of the decimation filter
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum DecimationBw {
    /// 100 Hz
    Hz100 = 0b00,
    /// 200 Hz
    Hz200 = 0b01,
    /// 400 Hz
    Hz400 = 0b10,
    /// 800 Hz
    Hz800 = 0b11,
}

impl DecimationBw {
    fn apply(self, reg: &mut AnalogControl) {
        reg.set_bw(self as u8);
    }

    pub(crate) fn delay_us(self) -> u32 {
        match self {
            DecimationBw::Hz100 => 8500,
            DecimationBw::Hz200 => 4500,
            DecimationBw::Hz400 => 2500,
            DecimationBw::Hz800 => 1000,
        }
    }
}

/// Frequency of measurements in continuous measurement mode
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ContinuousMeasurementFreq {
    /// Continuous Measurement Mode is off.
    Off = 0b000,
    /// 1 Hz
    Hz1 = 0b001,
    /// 10 Hz
    Hz10 = 0b010,
    /// 20 Hz
    Hz20 = 0b011,
    /// 50 Hz
    Hz50 = 0b100,
    /// 100 Hz
    Hz100 = 0b101,
    /// 200 Hz
    Hz200 = 0b110,
    /// 1000 Hz
    Hz1000 = 0b111,
}

impl ContinuousMeasurementFreq {
    fn apply(self, reg: &mut DigitalControl) {
        reg.set_cm_freq(self as u8);
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy)]
/// Periodically perform a SET operation.
pub enum PeriodicSetInterval {
    /// SET before every measurement
    Per1 = 0b000,
    /// SET every 25 measurements
    Per25 = 0b001,
    /// SET every 75 measurements
    Per75 = 0b010,
    /// SET every 100 measurements
    Per100 = 0b011,
    /// SET every 250 measurements
    Per250 = 0b100,
    /// SET every 500 measurements
    Per500 = 0b101,
    /// SET every 1000 measurements
    Per1000 = 0b110,
    /// SET every 2000 measurements
    Per2000 = 0b111,
    /// Disable periodic SET
    Off,
}

impl PeriodicSetInterval {
    fn apply(self, reg0b: &mut DigitalControl, reg09: &mut MeasurementTriggerControl) {
        match self {
            PeriodicSetInterval::Off => {
                reg09.set_auto_sr_en(false);
                reg0b.set_en_prd_set(false);
                reg0b.set_prd_set(0b000);
            }
            _ => {
                reg09.set_auto_sr_en(true);
                reg0b.set_en_prd_set(true);
                reg0b.set_prd_set(self as u8);
            }
        }
    }
}

/// Configuration for the MMC5983MA sensor
#[derive(Debug, Clone, Copy)]
pub struct Mmc5983Config {
    pub(crate) frequency: ContinuousMeasurementFreq,
    pub(crate) bandwidth: DecimationBw,
    set_interval: PeriodicSetInterval,
    inhibit: AxisInhibit,
    irq: bool,
}

/// Builds a configuration for the MMC5983MA sensor.
pub struct Mmc5983ConfigBuilder {
    frequency: ContinuousMeasurementFreq,
    bandwidth: DecimationBw,
    set_interval: PeriodicSetInterval,
    inhibit: AxisInhibit,
    irq: bool,
}

impl Default for Mmc5983ConfigBuilder {
    /// Returns a default configuration:
    /// - Frequency: 1 Hz
    /// - Bandwidth: 100 Hz
    /// - Periodic Set Interval: Per 100
    /// - Axis Inhibit: None (all axes enabled)
    /// - IRQ: Enabled
    fn default() -> Self {
        Mmc5983ConfigBuilder {
            frequency: ContinuousMeasurementFreq::Hz1,
            bandwidth: DecimationBw::Hz100,
            set_interval: PeriodicSetInterval::Per100,
            inhibit: AxisInhibit::None,
            irq: true,
        }
    }
}

impl Mmc5983ConfigBuilder {
    /// Sets the continuous measurement frequency.
    pub fn frequency(mut self, freq: ContinuousMeasurementFreq) -> Self {
        self.frequency = freq;
        self
    }

    /// Sets the bandwidth of the decimation filter.
    pub fn bandwidth(mut self, bw: DecimationBw) -> Self {
        self.bandwidth = bw;
        self
    }

    /// Sets the periodic set interval.
    pub fn set_interval(mut self, interval: PeriodicSetInterval) -> Self {
        self.set_interval = interval;
        self
    }

    /// Sets the axis inhibit configuration.
    pub fn inhibit(mut self, inhibit: AxisInhibit) -> Self {
        self.inhibit = inhibit;
        self
    }

    /// Enables or disables the IRQ pin.
    pub fn irq(mut self, enable: bool) -> Self {
        self.irq = enable;
        self
    }

    /// Builds the `Mmc5983Config` instance.
    pub fn build(self) -> Mmc5983Config {
        let bw = if self.frequency < ContinuousMeasurementFreq::Hz200 {
            self.bandwidth
        } else if self.frequency < ContinuousMeasurementFreq::Hz1000 {
            core::cmp::max(DecimationBw::Hz200, self.bandwidth)
        } else {
            DecimationBw::Hz800
        };

        Mmc5983Config {
            frequency: self.frequency,
            bandwidth: bw,
            set_interval: self.set_interval,
            inhibit: self.inhibit,
            irq: self.irq,
        }
    }
}

impl Mmc5983Config {
    pub(crate) fn to_registers(self) -> (AnalogControl, MeasurementTriggerControl, DigitalControl) {
        let mut analogctrl = AnalogControl::from(0);
        let mut digitalctrl = DigitalControl::from(0);
        let mut meastrigctrl = MeasurementTriggerControl::from(0);
        self.inhibit.apply(&mut analogctrl);
        self.set_interval.apply(&mut digitalctrl, &mut meastrigctrl);
        self.frequency.apply(&mut digitalctrl);
        self.bandwidth.apply(&mut analogctrl);
        if self.irq {
            meastrigctrl.set_drdy(true);
        }
        #[cfg(feature = "defmt-messages")]
        {
            use crate::debug;
            use crate::registers::Register;

            debug!(
                "Analog Control [0x{:02X}]: 0b{:08b}",
                AnalogControl::ADDRESS,
                analogctrl.to_u8()
            );
            debug!(
                "Digital Control [0x{:02X}]: 0b{:08b}",
                DigitalControl::ADDRESS,
                digitalctrl.to_u8()
            );
            debug!(
                "Measurement Trigger Control [0x{:02X}]: 0b{:08b}",
                MeasurementTriggerControl::ADDRESS,
                meastrigctrl.to_u8()
            );
        }
        (analogctrl, meastrigctrl, digitalctrl)
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Inhibit settings for each axis
pub enum AxisInhibit {
    /// All axes enabled
    None = 0x0,
    /// X axis disabled
    AxisX = 0b00100,
    /// Y and Z axis disabled
    AxisYZ = 0b11000,
    /// All axes disabled
    All = 0b11100,
}

impl AxisInhibit {
    fn apply(self, reg: &mut AnalogControl) {
        match self {
            AxisInhibit::None => {
                reg.set_x_inhibit(false);
                reg.set_yz_inhibit(0b00);
            }
            AxisInhibit::AxisX => reg.set_x_inhibit(true),
            AxisInhibit::AxisYZ => {
                reg.set_x_inhibit(false);
                reg.set_yz_inhibit(0b11);
            }
            AxisInhibit::All => {
                reg.set_x_inhibit(true);
                reg.set_yz_inhibit(0b11);
            }
        }
    }
}
