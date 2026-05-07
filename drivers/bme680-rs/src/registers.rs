#![allow(unused)]
use crate::Error;

use crate::interface::ReadPrimitive as _;

pub const CHIP_ID: u8 = 0xD0;
pub const RESET: u8 = 0xE0;
pub const VARIANT_ID: u8 = 0xF0;

// Calibration data – bank 1 (temperature + pressure)
pub const CALIB_BANK1_START: u8 = 0x89;
pub const CALIB_BANK1_LEN: u8 = 23;

// Calibration data – bank 2 (humidity + gas)
pub const CALIB_BANK2_START: u8 = 0xE1;
pub const CALIB_BANK2_LEN: u8 = 14;

// Control registers
pub const CTRL_HUM: u8 = 0x72;
pub const CTRL_MEAS: u8 = 0x74;
pub const CONFIG: u8 = 0x75;
pub const CTRL_GAS_0: u8 = 0x70;
pub const CTRL_GAS_1: u8 = 0x71;

// Gas heater set-point registers (index 0)
pub const RES_HEAT_0: u8 = 0x5A;
pub const GAS_WAIT_0: u8 = 0x64;

// Measurement result registers
pub const MEAS_STATUS_0: u8 = 0x1D;
pub const FIELD0_START: u8 = 0x1F;

#[derive(Default, Clone, Copy)]
pub(crate) struct TemperatureCalibration {
    pub t1: u16,
    pub t2: i16,
    pub t3: i8,
}

impl TemperatureCalibration {
    pub(crate) async fn read<IFACE>(iface: &mut IFACE) -> Result<Self, IFACE::Error>
    where
        IFACE: crate::interface::Interface,
    {
        let t1 = u16::read_primitive(iface, 0xe9).await?;
        let t2 = i16::read_primitive(iface, 0x8a).await?;
        let t3 = i8::read_primitive(iface, 0x8c).await?;
        Ok(Self { t1, t2, t3 })
    }
}

#[derive(Default, Clone, Copy)]
pub(crate) struct PressureCalibration {
    pub p1: u16,
    pub p2: i16,
    pub p3: i8,
    pub p4: i16,
    pub p5: i16,
    pub p6: i8,
    pub p7: i8,
    pub p8: i16,
    pub p9: i16,
    pub p10: u8,
}

impl PressureCalibration {
    pub(crate) async fn read<IFACE>(iface: &mut IFACE) -> Result<Self, IFACE::Error>
    where
        IFACE: crate::interface::Interface,
    {
        let p1 = u16::read_primitive(iface, 0x8e).await?;
        let p2 = i16::read_primitive(iface, 0x90).await?;
        let p3 = i8::read_primitive(iface, 0x92).await?;
        let p4 = i16::read_primitive(iface, 0x94).await?;
        let p5 = i16::read_primitive(iface, 0x96).await?;
        let p6 = i8::read_primitive(iface, 0x99).await?;
        let p7 = i8::read_primitive(iface, 0x98).await?;
        let p8 = i16::read_primitive(iface, 0x9c).await?;
        let p9 = i16::read_primitive(iface, 0x9e).await?;
        let p10 = u8::read_primitive(iface, 0xa0).await?;
        Ok(Self {
            p1,
            p2,
            p3,
            p4,
            p5,
            p6,
            p7,
            p8,
            p9,
            p10,
        })
    }
}

#[derive(Default, Clone, Copy)]
pub(crate) struct HumidityCalibration {
    pub h1: u16,
    pub h2: u16,
    pub h3: i8,
    pub h4: i8,
    pub h5: i8,
    pub h6: u8,
    pub h7: i8,
}

impl HumidityCalibration {
    pub(crate) async fn read<IFACE>(iface: &mut IFACE) -> Result<Self, IFACE::Error>
    where
        IFACE: crate::interface::Interface,
    {
        let mut buf = [0u8; 8];
        iface.read_registers(0xe1, &mut buf).await?;
        // H1 and H2 are 12-bit values packed across a shared register (0xE2).
        let h1 = (buf[2] as u16) << 4 | (buf[1] & 0x0F) as u16;
        let h2 = (buf[0] as u16) << 4 | (buf[1] >> 4) as u16;
        let h3 = buf[3] as i8;
        let h4 = buf[4] as i8;
        let h5 = buf[5] as i8;
        let h6 = buf[6];
        let h7 = buf[7] as i8;
        Ok(Self {
            h1,
            h2,
            h3,
            h4,
            h5,
            h6,
            h7,
        })
    }
}

#[derive(Default, Clone, Copy)]
pub(crate) struct GasCalibration {
    pub g1: i8,
    pub g2: i16,
    pub g3: i8,
    /// Heater resistance range correction (bits [5:4] of reg 0x02, divided by 4).
    pub res_heat_range: u8,
    /// Heater resistance value correction (reg 0x00, signed).
    pub res_heat_val: i8,
    /// Range switching error (upper nibble of reg 0x04, sign-extended and divided by 16).
    pub range_sw_err: i8,
}

impl GasCalibration {
    pub(crate) async fn read<IFACE>(iface: &mut IFACE) -> Result<Self, IFACE::Error>
    where
        IFACE: crate::interface::Interface,
    {
        let g1 = i8::read_primitive(iface, 0xed).await?;
        let g2 = i16::read_primitive(iface, 0xeb).await?;
        let g3 = i8::read_primitive(iface, 0xee).await?;
        // Coefficient group 3: registers 0x00–0x04.
        let mut coeff3 = [0u8; 5];
        iface.read_registers(0x00, &mut coeff3).await?;
        let res_heat_val = coeff3[0] as i8;
        let res_heat_range = (coeff3[2] & 0x30) >> 4;
        let range_sw_err = ((coeff3[4] & 0xF0) as i8) / 16;
        Ok(Self {
            g1,
            g2,
            g3,
            res_heat_range,
            res_heat_val,
            range_sw_err,
        })
    }
}

#[derive(Default, Clone, Copy)]
pub(crate) struct Calibration {
    pub temp: TemperatureCalibration,
    pub press: PressureCalibration,
    pub hum: HumidityCalibration,
    pub gas: GasCalibration,
}

impl Calibration {
    pub(crate) async fn read<IFACE>(iface: &mut IFACE) -> Result<Self, IFACE::Error>
    where
        IFACE: crate::interface::Interface,
    {
        Ok(Self {
            temp: TemperatureCalibration::read(iface).await?,
            press: PressureCalibration::read(iface).await?,
            hum: HumidityCalibration::read(iface).await?,
            gas: GasCalibration::read(iface).await?,
        })
    }
}

pub struct Reset {}

impl Reset {
    pub(crate) async fn reset<IFACE>(iface: &mut IFACE) -> Result<(), IFACE::Error>
    where
        IFACE: crate::interface::Interface,
    {
        iface.write_registers(RESET, &[0xB6]).await
    }
}

pub struct ChipId {}

impl ChipId {
    pub(crate) async fn read<IFACE>(iface: &mut IFACE) -> Result<(), Error<IFACE::Error>>
    where
        IFACE: crate::interface::Interface,
    {
        if u8::read_primitive(iface, CHIP_ID).await? != 0x61 {
            return Err(Error::InvalidChipId);
        }
        Ok(())
    }
}

#[repr(u8)]
#[bitfield_struct::bitenum]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Oversampling settings for temperature, pressure, and humidity measurements.
pub enum Oversampling {
    #[fallback]
    /// No oversampling (output will be zero).
    None = 0,
    /// Oversampling x1 (default).
    X1 = 1,
    /// Oversampling x2.
    X2 = 2,
    /// Oversampling x4.
    X4 = 3,
    /// Oversampling x8.
    X8 = 4,
    /// Oversampling x16.
    X16 = 5,
}

impl Oversampling {
    /// Returns the number of samples taken (0 when disabled).
    pub(crate) const fn samples(self) -> u32 {
        match self {
            Oversampling::None => 0,
            Oversampling::X1 => 1,
            Oversampling::X2 => 2,
            Oversampling::X4 => 4,
            Oversampling::X8 => 8,
            Oversampling::X16 => 16,
        }
    }
}

#[bitfield_struct::bitenum]
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Measurement modes for the BME680.
pub enum MeasurementMode {
    #[fallback]
    /// Sleep mode (default). No measurements are performed, and the sensor is in low-power standby.
    Sleep = 0,
    /// Forced mode. Perform a single measurement and return to sleep mode.
    Forced = 1,
}

#[bitfield_struct::bitfield(u8)]
pub struct CtrlTempPres {
    #[bits(2)]
    pub mode: MeasurementMode,
    #[bits(3)]
    pub pressure: Oversampling,
    #[bits(3)]
    pub temperature: Oversampling,
}

#[bitfield_struct::bitfield(u8)]
pub struct CtrlHumGas {
    #[bits(3)]
    pub humidity: Oversampling,
    #[bits(5, default = 0)]
    _rsvd: u8,
}

#[bitfield_struct::bitfield(u8)]
pub struct CtrlGas1 {
    #[bits(4)]
    pub index: u8,
    #[bits(1)]
    pub run_gas: bool,
    #[bits(3, default = 0)]
    _rsvd: u8,
}

#[bitfield_struct::bitfield(u8)]
pub struct CtrlGas0 {
    #[bits(3)]
    _rsvd: u8,
    #[bits(1)]
    pub heat_off: bool,
    #[bits(4)]
    _rsvd2: u8,
}

#[bitfield_struct::bitfield(u16)]
pub struct GasValue {
    #[bits(4)]
    pub range: u8,
    #[bits(1)]
    pub stable: bool,
    #[bits(1)]
    pub valid: bool,
    #[bits(10)]
    pub value: u16,
}

#[bitfield_struct::bitfield(u8)]
pub struct Status {
    #[bits(4)]
    pub index: u8,
    #[bits(1)]
    rsv: bool,
    #[bits(1)]
    measuring: bool,
    #[bits(1)]
    gas_measuring: bool,
    #[bits(1)]
    pub new_data: bool,
}

/// Compensate a raw 20-bit temperature ADC value.
///
/// Stores the intermediate `t_fine` value (used by humidity compensation)
/// and returns temperature in degrees Celsius.
pub(crate) fn compensate_temperature(
    adc_temp: u32,
    t_fine: &mut f32,
    c: &TemperatureCalibration,
) -> f32 {
    let adc = adc_temp as f32;
    let t1 = c.t1 as f32;
    let var1 = (adc / 16384.0 - t1 / 1024.0) * c.t2 as f32;
    let partial = adc / 131072.0 - t1 / 8192.0;
    let var2 = partial * partial * (c.t3 as f32 * 16.0);
    *t_fine = var1 + var2;
    *t_fine / 5120.0
}

/// Compensate a raw 20-bit pressure ADC value.
///
/// Requires the `t_fine` value produced by [`compensate_temperature`].
/// Returns pressure in Pascals. Returns 0.0 if the IIR divisor would be zero.
pub(crate) fn compensate_pressure(adc_pres: u32, t_fine: f32, c: &PressureCalibration) -> f32 {
    let var1 = t_fine / 2.0 - 64000.0;
    let var2 = var1 * var1 * (c.p6 as f32 / 131072.0) + var1 * (c.p5 as f32 * 2.0);
    let var2 = var2 / 4.0 + c.p4 as f32 * 65536.0;
    let var1 = ((c.p3 as f32 * var1 * var1 / 16384.0) + (c.p2 as f32 * var1)) / 524288.0;
    let var1 = (1.0 + var1 / 32768.0) * c.p1 as f32;
    if var1 == 0.0 {
        return 0.0;
    }
    let mut p = 1_048_576.0_f32 - adc_pres as f32;
    p = ((p - var2 / 4096.0) * 6250.0) / var1;
    let var1 = c.p9 as f32 * p * p / 2_147_483_648.0;
    let var2 = p * (c.p8 as f32 / 32768.0);
    let var3 = (p / 256.0) * (p / 256.0) * (p / 256.0) * (c.p10 as f32 / 131072.0);
    p + (var1 + var2 + var3 + c.p7 as f32 * 128.0) / 16.0
}

/// Compensate a raw 16-bit humidity ADC value.
///
/// Requires the `t_fine` value produced by [`compensate_temperature`].
/// Returns relative humidity in percent (0.0–100.0).
pub(crate) fn compensate_humidity(adc_hum: u16, t_fine: f32, c: &HumidityCalibration) -> f32 {
    let temp_comp = t_fine / 5120.0;
    let var1 = adc_hum as f32 - (c.h1 as f32 * 16.0 + (c.h3 as f32 / 2.0) * temp_comp);
    let var2 = var1
        * (c.h2 as f32 / 262144.0
            * (1.0
                + (c.h4 as f32 / 16384.0) * temp_comp
                + (c.h5 as f32 / 1048576.0) * temp_comp * temp_comp));
    let var3 = c.h6 as f32 / 16384.0;
    let var4 = c.h7 as f32 / 2097152.0;
    let comp_hum = var2 + (var3 + var4 * temp_comp) * var2 * var2;
    comp_hum.clamp(0.0, 100.0)
}

/// Compensate a raw 10-bit gas ADC value for the BME680 (low gas variant).
///
/// Returns gas resistance in Ohms.
pub(crate) fn compensate_gas_resistance_low(
    adc_gas: u16,
    gas_range: u8,
    c: &GasCalibration,
) -> f32 {
    const LOOKUP_K1: [f32; 16] = [
        0.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, -0.8, 0.0, 0.0, -0.2, -0.5, 0.0, -1.0, 0.0, 0.0,
    ];
    const LOOKUP_K2: [f32; 16] = [
        0.0, 0.0, 0.0, 0.0, 0.1, 0.7, 0.0, -0.8, -0.1, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
    ];
    let idx = (gas_range & 0x0F) as usize;
    let gas_range_f = (1u32 << gas_range) as f32;
    let var1 = 1340.0_f32 + 5.0 * c.range_sw_err as f32;
    let var2 = var1 * (1.0 + LOOKUP_K1[idx] / 100.0);
    let var3 = 1.0_f32 + LOOKUP_K2[idx] / 100.0;
    1.0 / (var3 * 0.000_000_125_f32 * gas_range_f * ((adc_gas as f32 - 512.0) / var2 + 1.0))
}

/// Compute the RES_HEAT register value for a target heater temperature.
///
/// `target_temp`: desired temperature in °C (capped at 400 °C).
/// `amb_temp`: ambient temperature in °C (25 is a safe default).
pub(crate) fn calc_res_heat(target_temp: u16, amb_temp: i8, c: &GasCalibration) -> u8 {
    let target = target_temp.min(400) as f32;
    let amb = amb_temp as f32;
    let var1 = c.g1 as f32 / 16.0 + 49.0;
    let var2 = c.g2 as f32 / 32768.0 * 0.0005 + 0.00235;
    let var3 = c.g3 as f32 / 1024.0;
    let var4 = var1 * (1.0 + var2 * target);
    let var5 = var4 + var3 * amb;
    let result = 3.4
        * (var5
            * (4.0 / (4.0 + c.res_heat_range as f32))
            * (1.0 / (1.0 + c.res_heat_val as f32 * 0.002))
            - 25.0);
    result.clamp(0.0, 255.0) as u8
}

/// Encode a heater on-time in milliseconds into the GAS_WAIT register format.
pub(crate) fn calc_gas_wait(dur_ms: u16) -> u8 {
    if dur_ms >= 0xfc0 {
        return 0xff;
    }
    let mut dur = dur_ms;
    let mut factor: u8 = 0;
    while dur > 0x3f {
        dur /= 4;
        factor += 1;
    }
    dur as u8 + factor * 64
}
