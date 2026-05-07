use crate::info;
use bitfield_struct::bitfield;

pub(crate) const BMP390_DEVICE_ID: u8 = 0x60;

pub(crate) trait Register {
    const ADDRESS: u8;
    fn from_u8(value: u8) -> Self
    where
        Self: Sized;
    fn to_u8(&self) -> u8;
}

macro_rules! impl_register {
    ($reg:ident, $addr:expr) => {
        impl Register for $reg {
            const ADDRESS: u8 = $addr;
            #[inline(always)]
            fn from_u8(value: u8) -> Self {
                Self::from(value)
            }
            #[inline(always)]
            fn to_u8(&self) -> u8 {
                self.0
            }
        }
    };
    ($addr:expr, $reg:ident) => {
        impl Register for $reg {
            const ADDRESS: u8 = $addr;
            #[inline(always)]
            fn from_u8(value: u8) -> Self {
                Self { 0: value }
            }
            #[inline(always)]
            fn to_u8(&self) -> u8 {
                self.0
            }
        }
    };
}

/// Device ID register, address 0x0
pub(crate) struct DeviceId(pub(crate) u8);
impl_register!(0x0, DeviceId);

impl DeviceId {
    pub(crate) fn validate(&self) -> bool {
        {
            info!("Device ID read: {:#x}", self.0);
        }
        self.0 == BMP390_DEVICE_ID
    }
}

#[bitfield(u8)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Sensor Error Register, address 0x02
pub(crate) struct ErrorReg {
    #[bits(1, access=RO)]
    /// Fatal error, chip is not in operatonal state (boot-, power-system).
    /// This flag will be reset only by PoR or soft-reset.
    pub fatal: bool,
    #[bits(1, access=RO)]
    /// Command error detected.
    pub command: bool,
    #[bits(1, access=RO)]
    /// Invalid configuration detected.
    pub configuration: bool,
    #[bits(5, default = false)]
    _rsvd: u8,
}

impl_register!(ErrorReg, 0x02);

#[bitfield(u8)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Sensor Status Register, address 0x03
pub(crate) struct StatusReg {
    #[bits(4, default = 0)]
    _rsvd1: u8,
    #[bits(1, access=RO)]
    /// Command ready flag.
    cmd_ready: bool,
    #[bits(1, access=RO)]
    /// Data ready flag for pressure dataa
    pub press_drdy: bool,
    #[bits(1, access=RO)]
    /// Data ready flag for temperature data
    pub temp_drdy: bool,
    #[bits(1, default = false)]
    _rsvd2: u8,
}

impl_register!(StatusReg, 0x03);

/// 3 bytes of pressure data, in LE format.
pub(crate) const PRESSURE_DATA_ADDR: u8 = 0x4;
#[allow(unused)]
/// 3 bytes of temperature data, in LE format
pub(crate) const TEMP_DATA_ADDR: u8 = 0x07;
/// 3-bytes of time data, in LE format
#[allow(unused)]
pub(crate) const TIME_DATA_ADDR: u8 = 0x0C;

#[bitfield(u8)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Event register, address 0x10
pub(crate) struct EventReg {
    #[bits(1, access=RO)]
    /// true after a power-on-reset (PoR) or soft-reset. Clears after read.
    pub por: bool,
    #[bits(1, access=RO)]
    /// true when a serial interface transaction occurs during a pressure or temperature measurement. Clears after read.
    pub data_ready: bool,
    #[bits(6, default = 0)]
    _rsvd: u8,
}

impl_register!(EventReg, 0x10);

/// Interrupt status, address 0x11
/// This register is clear-on-read.
#[bitfield(u8)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct IrqStatus {
    #[bits(1, access=RO)]
    pub fifo_watermark: bool,
    #[bits(1, access=RO)]
    pub fifo_full: bool,
    #[bits(1, default = false)]
    _rsvd1: bool,
    #[bits(1, access=RO)]
    /// Data ready interrupt status
    pub drdy: bool,
    #[bits(4, default = 0)]
    _rsvd: u8,
}

impl_register!(IrqStatus, 0x11);

/// Interrupt control register, address 0x19
#[bitfield(u8)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct IrqControl {
    #[bits(1, access=RW, default = false)]
    pub open_drain: bool,
    #[bits(1, access=RW, default = false)]
    pub active_high: bool,
    #[bits(1, access=RW, default = false)]
    pub latching: bool,
    #[bits(1, access=RW, default = false)]
    pub fifo_watermark: bool,
    #[bits(1, access=RW, default = false)]
    pub fifo_full: bool,
    #[bits(1, access=RW, default = false)]
    pub ds: bool,
    #[bits(1, access=RW, default = false)]
    pub drdy: bool,
    #[bits(1, default = 0)]
    _rsvd: u8,
}

impl_register!(IrqControl, 0x19);

/// Power control, addres 0x1b
#[bitfield(u8)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct PowerCtrl {
    #[bits(1, access=RW, default = false)]
    /// Enable or disable the pressure sensor
    pub press_en: bool,
    #[bits(1, access=RW, default = false)]
    /// Enable or disable the temperature sensor
    pub temp_en: bool,
    #[bits(2, default = 0)]
    _rsvd1: u8,
    #[bits(2, access=RW, from = SensorMode::from_u8)]
    /// Sensor mode
    pub mode: SensorMode,
    #[bits(2, default = 0)]
    _rsvd2: u8,
}

impl_register!(PowerCtrl, 0x1B);

/// Sensor mode
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum SensorMode {
    /// Sleep mode
    Sleep = 0x0,
    /// Single measurement mode
    Forced = 0x1,
    /// Normal mode
    #[default]
    Normal = 0x3,
}

impl SensorMode {
    pub(crate) const fn into_bits(self) -> u8 {
        self as u8
    }
    pub(crate) const fn from_u8(value: u8) -> Self {
        match value {
            0x0 => Self::Sleep,
            0x1 => Self::Forced,
            0x3 => Self::Normal,
            _ => Self::Sleep, // Default to Sleep
        }
    }
}

/// Oversampling settings
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Oversampling {
    /// Skipped (output set to 0x80000 for pressure, 0x8000 for temperature)
    #[default]
    Skip = 0x0,
    /// 2x oversampling
    X2 = 0x1,
    /// 4x oversampling
    X4 = 0x2,
    /// 8x oversampling
    X8 = 0x3,
    /// 16x oversampling
    X16 = 0x4,
    /// 32x oversampling
    X32 = 0x5,
}

impl Oversampling {
    pub(crate) const fn into_bits(self) -> u8 {
        self as u8
    }
    pub(crate) const fn from_u8(value: u8) -> Self {
        match value {
            0x0 => Self::Skip,
            0x1 => Self::X2,
            0x2 => Self::X4,
            0x3 => Self::X8,
            0x4 => Self::X16,
            0x5 => Self::X32,
            _ => Self::Skip, // Default to Skip
        }
    }
}

#[bitfield(u8)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Oversampling settings register, address 0x1C
pub struct OversamplingReg {
    #[bits(3, access=RW, from = Oversampling::from_u8, default = Oversampling::X16)]
    /// Pressure oversampling
    pub pressure: Oversampling,
    #[bits(3, access=RW, from = Oversampling::from_u8, default = Oversampling::X2)]
    /// Temperature oversampling
    pub temperature: Oversampling,
    #[bits(2, default = 0)]
    _rsvd: u8,
}

impl_register!(OversamplingReg, 0x1C);

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Output data rate settings
pub enum OutputDataRate {
    /// 200 Hz, 5 ms
    Hz200 = 0x0,
    /// 100 Hz, 10 ms
    Hz100 = 0x1,
    /// 50 Hz, 20 ms
    Hz50 = 0x2,
    /// 25 Hz, 40 ms
    #[default]
    Hz25 = 0x3,
    /// 25 / 2 Hz, 80 ms
    Hz25_2 = 0x4,
    /// 25 / 4 Hz, 160 ms
    Hz25_4 = 0x5,
    /// 25 / 8 Hz, 320 ms
    Hz25_8 = 0x6,
    /// 25 / 16 Hz, 640 ms
    Hz25_16 = 0x7,
    /// 25 / 32 Hz, 1280 ms
    Hz25_32 = 0x8,
    /// 25 / 64 Hz, 2560 ms
    Hz25_64 = 0x9,
    /// 25 / 128 Hz, 5120 ms
    Hz25_128 = 0xA,
    /// 25 / 256 Hz, 10240 ms
    Hz25_256 = 0xB,
    /// 25 / 512 Hz, 20480 ms
    Hz25_512 = 0xC,
    /// 25 / 1024 Hz, 40960 ms
    Hz25_1024 = 0xD,
    /// 25 / 2048 Hz, 81920 ms
    Hz25_2048 = 0xE,
    /// 25 / 4096 Hz, 163840 ms
    Hz25_4096 = 0xF,
    /// 25 / 8192 Hz, 327680 ms
    Hz25_8192 = 0x10,
    /// 25 / 16384 Hz, 655360 ms
    Hz25_16384 = 0x11,
}

impl OutputDataRate {
    pub(crate) const fn from_u8(value: u8) -> Self {
        match value {
            0x0 => Self::Hz200,
            0x1 => Self::Hz100,
            0x2 => Self::Hz50,
            0x3 => Self::Hz25,
            0x4 => Self::Hz25_2,
            0x5 => Self::Hz25_4,
            0x6 => Self::Hz25_8,
            0x7 => Self::Hz25_16,
            0x8 => Self::Hz25_32,
            0x9 => Self::Hz25_64,
            0xA => Self::Hz25_128,
            0xB => Self::Hz25_256,
            0xC => Self::Hz25_512,
            0xD => Self::Hz25_1024,
            0xE => Self::Hz25_2048,
            0xF => Self::Hz25_4096,
            0x10 => Self::Hz25_8192,
            0x11 => Self::Hz25_16384,
            _ => Self::Hz100, // Default to 100 Hz
        }
    }

    /// Sample delay in microseconds
    pub const fn delay(&self) -> u32 {
        match self {
            Self::Hz200 => 5_000,
            Self::Hz100 => 10_000,
            Self::Hz50 => 20_000,
            Self::Hz25 => 40_000,
            Self::Hz25_2 => 80_000,
            Self::Hz25_4 => 160_000,
            Self::Hz25_8 => 320_000,
            Self::Hz25_16 => 640_000,
            Self::Hz25_32 => 1_280_000,
            Self::Hz25_64 => 2_560_000,
            Self::Hz25_128 => 5_120_000,
            Self::Hz25_256 => 10_240_000,
            Self::Hz25_512 => 20_480_000,
            Self::Hz25_1024 => 40_960_000,
            Self::Hz25_2048 => 81_920_000,
            Self::Hz25_4096 => 163_840_000,
            Self::Hz25_8192 => 327_680_000,
            Self::Hz25_16384 => 655_360_000,
        }
    }
}

impl Register for OutputDataRate {
    const ADDRESS: u8 = 0x1d;
    #[inline(always)]
    fn from_u8(value: u8) -> Self {
        Self::from_u8(value)
    }
    #[inline(always)]
    fn to_u8(&self) -> u8 {
        *self as u8
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// IIR filter coefficients
pub enum IIRFilterConfig {
    /// Bypass (filter disabled)
    #[default]
    Bypass = 0x0,
    #[allow(missing_docs)]
    Coeff1 = 0x2,
    #[allow(missing_docs)]
    Coeff3 = 0x4,
    #[allow(missing_docs)]
    Coeff7 = 0x6,
    #[allow(missing_docs)]
    Coeff15 = 0x8,
    #[allow(missing_docs)]
    Coeff31 = 0xA,
    #[allow(missing_docs)]
    Coeff63 = 0xC,
    #[allow(missing_docs)]
    Coeff127 = 0xE,
}

impl IIRFilterConfig {
    pub(crate) const fn from_u8(value: u8) -> Self {
        match value {
            0x0 => Self::Bypass,
            0x1 => Self::Coeff1,
            0x2 => Self::Coeff3,
            0x3 => Self::Coeff7,
            0x4 => Self::Coeff15,
            0x5 => Self::Coeff31,
            0x6 => Self::Coeff63,
            0x7 => Self::Coeff127,
            _ => Self::Bypass, // Default to Bypass
        }
    }
}

impl Register for IIRFilterConfig {
    const ADDRESS: u8 = 0x1f;
    #[inline(always)]
    fn from_u8(value: u8) -> Self {
        Self::from_u8(value)
    }
    #[inline(always)]
    fn to_u8(&self) -> u8 {
        *self as u8
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd)]
/// Command register, address 0x7E
pub(crate) enum Command {
    /// No operation
    Nop = 0x0,
    /// Flush FIFO
    FifoFlush = 0xB0,
    /// Reset the device. All registers are set to default values.
    SoftReset = 0xb6,
}

impl From<u8> for Command {
    fn from(value: u8) -> Self {
        match value {
            0x0 => Self::Nop,
            0xB0 => Self::FifoFlush,
            0xB6 => Self::SoftReset,
            _ => Self::Nop, // Default to Nop
        }
    }
}

impl Register for Command {
    const ADDRESS: u8 = 0x7E;
    #[inline(always)]
    fn from_u8(value: u8) -> Self {
        Self::from(value)
    }
    #[inline(always)]
    fn to_u8(&self) -> u8 {
        *self as u8
    }
}

/// Calibration coefficient registers start address
/// These are 21 bytes long, from 0x31 to 0x45
pub(crate) const NVM_PAR_T1_0: u8 = 0x31;
