#[cfg(feature = "defmt")]
use crate::trace;
use bitfield_struct::bitfield;

pub(crate) const BMI323_DEVICE_ID: u16 = 0x43;

pub(crate) trait Register {
    const ADDRESS: u8;
    fn from_u16(value: u16) -> Self
    where
        Self: Sized;
    fn to_u16(&self) -> u16;
}

macro_rules! impl_register {
    ($reg:ident, $addr:expr) => {
        impl Register for $reg {
            const ADDRESS: u8 = $addr;
            #[inline(always)]
            fn from_u16(value: u16) -> Self {
                Self::from(value)
            }
            #[inline(always)]
            fn to_u16(&self) -> u16 {
                self.0
            }
        }
    };
    ($addr:expr, $reg:ident) => {
        impl Register for $reg {
            const ADDRESS: u8 = $addr;
            #[inline(always)]
            fn from_u16(value: u16) -> Self {
                Self { 0: value }
            }
            #[inline(always)]
            fn to_u16(&self) -> u16 {
                self.0
            }
        }
    };
}

/// Device ID register, address 0x43
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct DeviceId(pub(crate) u16);
impl_register!(0x0, DeviceId);

impl DeviceId {
    pub(crate) fn validate(&self) -> bool {
        #[cfg(feature = "defmt")]
        {
            trace!("Device ID read: {:#x}", self.0);
        }
        self.0 & 0x00ff == BMI323_DEVICE_ID
    }
}

#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Sensor Error Register, address 0x01
pub(crate) struct ErrorReg {
    #[bits(1, access=RO)]
    /// Fatal error, chip is not in operatonal state (boot-, power-system).
    /// This flag will be reset only by PoR or soft-reset.
    pub fatal: bool,
    #[bits(1, default = false)]
    _rsvd1: bool,
    #[bits(1, access=RW)]
    /// Overload of the feature engine detected.
    /// Flag is clear-on-read.
    pub fteng_ovld: bool,
    #[bits(1, default = false)]
    _rsvd2: u8,
    /// Watchdog timer of the feature engine triggered.
    /// Flag is clear-on-read.
    #[bits(1, access=RW)]
    pub fteng_wat: bool,
    #[bits(1, access=RO)]
    /// Unsupported accelerometer configuration set.
    /// This flag is reset when correct configuration is applied.
    pub accel_conf: bool,
    #[bits(1, access=RO)]
    /// Unsupported gyroscope configuration error.
    /// This flag is reset when correct configuration is applied.
    pub gyro_conf: bool,
    #[bits(1, default = false)]
    _rsvd3: u8,
    #[bits(1, access=RO)]
    /// SDR parity error or read abort condition (max. clock stall time for I3C read transfer) occurred.
    /// Flag is clear-on-read. Refer to MIPI I3C 'Master Clock Stalling' for read abort conditions.
    pub i3c_error0: bool,
    #[bits(2, default = false)]
    _rsvd4: u8,
    #[bits(1, access=RO)]
    /// S0/S1 error occurred. The slave will recover automatically after 60us
    /// Flag is clear on read.
    /// Flag persists for notification on a HDR-exit pattern.
    pub i3c_error3: bool,
    #[bits(4, default = false)]
    _rsvd5: u8,
}

impl ErrorReg {
    /// Check if a fatal error has occurred.
    #[inline(always)]
    pub fn sensor_fatal(&self) -> bool {
        self.fatal() || self.accel_conf() || self.gyro_conf()
    }
}

impl_register!(ErrorReg, 0x01);

#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Sensor Status Register, address 0x02
pub(crate) struct StatusReg {
    #[bits(1, access=RW)]
    /// ’1’ after device power up or soft-reset. This ﬂag is clear-on-read.
    por_detect: bool,
    #[bits(4, default = 0)]
    _rsvd1: u8,
    #[bits(1, access=RW)]
    /// Data ready flag for temperature data.
    /// Flag is clear-on-read.
    pub(crate) drdy_temp: bool,
    #[bits(1, access=RW)]
    /// Data ready flag for gyroscope data.
    /// Flag is clear-on-read.
    pub(crate) drdy_gyr: bool,
    #[bits(1, access=RW)]
    /// Data ready flag for accelerometer data.
    /// Flag is clear-on-read.
    pub(crate) drdy_acc: bool,
    #[bits(8, default = 0)]
    _rsvd2: u8,
}

impl_register!(StatusReg, 0x02);

/// 6-bytes of accelerometer data, in LE format, XYZ
pub(crate) const ACCEL_DATA_ADDR: u8 = 0x3;
/// 6-bytes of gyroscope data, in LE format, XYZ
#[allow(unused)]
pub(crate) const GYRO_DATA_ADDR: u8 = 0x06;
/// 2-bytes of temperature data, in LE format.
/// Resolution is 512LSB/°C with 0x0000 = 23°C, 0x8000 -> invalid
#[allow(unused)]
pub(crate) const TEMP_DATA_ADDR: u8 = 0x09;
/// 4-bytes of time data, in LE format
#[allow(unused)]
pub(crate) const TIME_DATA_ADDR: u8 = 0x0A;

#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Sensor saturation register, address 0x0C
pub(crate) struct SaturationReg {
    #[bits(1, access=RO)]
    /// Accelerometer X-axis saturation flag.
    pub acc_x: bool,
    #[bits(1, access=RO)]
    /// Accelerometer Y-axis saturation flag.
    pub acc_y: bool,
    #[bits(1, access=RO)]
    /// Accelerometer Z-axis saturation flag.
    pub acc_z: bool,
    #[bits(1, access=RO)]
    /// Gyroscope X-axis saturation flag.
    pub gyr_x: bool,
    #[bits(1, access=RO)]
    /// Gyroscope Y-axis saturation flag.
    pub gyr_y: bool,
    #[bits(1, access=RO)]
    /// Gyroscope Z-axis saturation flag.
    pub gyr_z: bool,
    #[bits(10, default = 0)]
    _rsvd: u16,
}

impl_register!(SaturationReg, 0x0C);

/// Interrupt status for INT1, address 0x0D
/// This register is clear-on-read.
#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct Int1Status {
    #[bits(1, access=RO)]
    /// No motion interrupt status
    pub no_motion: bool,
    #[bits(1, access=RO)]
    /// Any motion interrupt status
    pub any_motion: bool,
    #[bits(1, access=RO)]
    /// Flat detection interrupt status
    pub flat: bool,
    #[bits(1, access=RO)]
    /// Orientation interrupt status
    pub orientation: bool,
    #[bits(1, access=RO)]
    /// Step detector interrupt status
    pub step_det: bool,
    #[bits(1, access=RO)]
    /// Step counter interrupt status
    pub step_count: bool,
    #[bits(1, access=RO)]
    /// Significant motion interrupt status
    pub sig_motion: bool,
    #[bits(1, access=RO)]
    /// Tilt interrupt status
    pub tilt: bool,
    #[bits(1, access=RO)]
    /// Tap detection interrupt status
    pub tap: bool,
    #[bits(1, access=RO)]
    /// I3C sync data ready interrupt status
    pub i3c_sync_drdy: bool,
    #[bits(1, access=RO)]
    /// Feature engine error or status change
    pub feat_eng: bool,
    #[bits(1, access=RO)]
    /// Temperature data ready interrupt status
    pub temp_drdy: bool,
    #[bits(1, access=RO)]
    /// Gyroscope data ready interrupt status
    pub gyr_drdy: bool,
    #[bits(1, access=RO)]
    /// Accelerometer data ready interrupt status
    pub acc_drdy: bool,
    #[bits(1, access=RO)]
    pub fifo_watermark: bool,
    #[bits(1, access=RO)]
    pub fifo_full: bool,
}

impl_register!(Int1Status, 0x0D);

/// Interrupt status for INT2, address 0x0E.
/// This register is clear-on-read.
#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct Int2Status {
    #[bits(1, access=RO)]
    /// No motion interrupt status
    pub no_motion: bool,
    #[bits(1, access=RO)]
    /// Any motion interrupt status
    pub any_motion: bool,
    #[bits(1, access=RO)]
    /// Flat detection interrupt status
    pub flat: bool,
    #[bits(1, access=RO)]
    /// Orientation interrupt status
    pub orientation: bool,
    #[bits(1, access=RO)]
    /// Step detector interrupt status
    pub step_det: bool,
    #[bits(1, access=RO)]
    /// Step counter interrupt status
    pub step_count: bool,
    #[bits(1, access=RO)]
    /// Significant motion interrupt status
    pub sig_motion: bool,
    #[bits(1, access=RO)]
    /// Tilt interrupt status
    pub tilt: bool,
    #[bits(1, access=RO)]
    /// Tap detection interrupt status
    pub tap: bool,
    #[bits(1, access=RO)]
    /// I3C sync data ready interrupt status
    pub i3c_sync_drdy: bool,
    #[bits(1, access=RO)]
    /// Feature engine error or status change
    pub feat_eng: bool,
    #[bits(1, access=RO)]
    /// Temperature data ready interrupt status
    pub temp_drdy: bool,
    #[bits(1, access=RO)]
    /// Gyroscope data ready interrupt status
    pub gyr_drdy: bool,
    #[bits(1, access=RO)]
    /// Accelerometer data ready interrupt status
    pub acc_drdy: bool,
    #[bits(1, access=RO)]
    pub fifo_watermark: bool,
    #[bits(1, access=RO)]
    pub fifo_full: bool,
}

impl_register!(Int2Status, 0x0E);

/// Interrupt status for I3C IBI, address 0x0F.
/// This register is clear-on-read.
#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct IbiStatus {
    #[bits(1, access=RO)]
    /// No motion interrupt status
    pub no_motion: bool,
    #[bits(1, access=RO)]
    /// Any motion interrupt status
    pub any_motion: bool,
    #[bits(1, access=RO)]
    /// Flat detection interrupt status
    pub flat: bool,
    #[bits(1, access=RO)]
    /// Orientation interrupt status
    pub orientation: bool,
    #[bits(1, access=RO)]
    /// Step detector interrupt status
    pub step_det: bool,
    #[bits(1, access=RO)]
    /// Step counter interrupt status
    pub step_count: bool,
    #[bits(1, access=RO)]
    /// Significant motion interrupt status
    pub sig_motion: bool,
    #[bits(1, access=RO)]
    /// Tilt interrupt status
    pub tilt: bool,
    #[bits(1, access=RO)]
    /// Tap detection interrupt status
    pub tap: bool,
    #[bits(1, access=RO)]
    /// I3C sync data ready interrupt status
    pub i3c_sync_drdy: bool,
    #[bits(1, access=RO)]
    /// Feature engine error or status change
    pub feat_eng: bool,
    #[bits(1, access=RO)]
    /// Temperature data ready interrupt status
    pub temp_drdy: bool,
    #[bits(1, access=RO)]
    /// Gyroscope data ready interrupt status
    pub gyr_drdy: bool,
    #[bits(1, access=RO)]
    /// Accelerometer data ready interrupt status
    pub acc_drdy: bool,
    #[bits(1, access=RO)]
    pub fifo_watermark: bool,
    #[bits(1, access=RO)]
    pub fifo_full: bool,
}

impl_register!(IbiStatus, 0x0F);

#[bitfield(u16)]
/// Feature engine configuration.
/// Before setting/changing an active configuration, the register must be cleared.
/// Address 0x10
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct FeatEngConfig {
    #[bits(1, access=RW, default=false)]
    /// Enable no-motion feature for X-axis
    pub no_motion_x: bool,
    #[bits(1, access=RW, default=false)]
    /// Enable no-motion feature for Y-axis
    pub no_motion_y: bool,
    #[bits(1, access=RW, default=false)]
    /// Enable no-motion feature for Z-axis
    pub no_motion_z: bool,
    #[bits(1, access=RW, default=false)]
    /// Enable any-motion feature for X-axis
    pub any_motion_x: bool,
    #[bits(1, access=RW, default=false)]
    /// Enable any-motion feature for Y-axis
    pub any_motion_y: bool,
    #[bits(1, access=RW, default=false)]
    /// Enable any-motion feature for Z-axis
    pub any_motion_z: bool,
    #[bits(1, access=RW, default=false)]
    /// Enable flat detection feature
    pub flat: bool,
    #[bits(1, access=RW, default=false)]
    /// Enable orientation feature
    pub orientation: bool,
    #[bits(1, access=RW, default=false)]
    /// Enable step detector feature
    pub step_det: bool,
    #[bits(1, access=RW, default=false)]
    /// Enable step counter feature
    pub step_count: bool,
    #[bits(1, access=RW, default=false)]
    /// Enable significant motion feature
    pub sig_motion: bool,
    #[bits(1, access=RW, default=false)]
    /// Enable tilt feature
    pub tilt: bool,
    #[bits(1, access=RW, default=false)]
    /// Enable single tap feature
    pub tap_single: bool,
    #[bits(1, access=RW, default=false)]
    /// Enable double tap feature
    pub tap_double: bool,
    #[bits(1, access=RW, default=false)]
    /// Enable triple tap feature
    pub tap_triple: bool,
    #[bits(1, access=RW, default=false)]
    /// Enable I3C sync feature
    pub i3c_sync: bool,
}

impl_register!(FeatEngConfig, 0x10);

#[repr(u8)]
#[derive(Debug, PartialEq, Eq, Ord, PartialOrd)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) enum FeatureIo1Error {
    /// Feature engine is still active
    Active = 0x0,
    /// Feature engine activated
    Activated = 0x1,
    /// Configuration string download failed
    ConfigDwnldFailed = 0x3,
    /// No error
    NoError = 0x5,
    /// Axis map command was not processed because
    /// either a sensor was active or self-calibration
    /// or self-testing was on-going
    AxisMapFailed = 0x6,
    /// I3C TC-sync error because
    /// - TC-sync enable was requeseted while auto-low-power feature was active
    /// - TC-sync configuration command was sent with invalid TPH, TU and oDR values
    ///   Invalid configuration parameters will not be used.
    I3cTcSyncError = 0x8,
    /// Gyroscope self-calibration or self-test was aborted.
    GyroSelfAborted = 0x9,
    /// Gyroscope self-calibration or self-test command ignored.
    GyroSelfIgnored = 0xA,
    /// Accelerometer self-calibration or self-test was aborted.
    AccelSelfAborted = 0xB,
    /// Accelerometer self-calibration or self-test command ignored.
    AccelSelfIgnored = 0xC,
    /// Auto-mode change feature was enabled or illegal sensor configuration change
    /// detected in [`AccelCfg`] or [`GyroCfg`] register while self-calibration or self-test
    /// was on-going. Self-calibration or self-test results may be inaccurate.
    AutoModeChange = 0xD,
    /// I3C TC-sync enable request was sent while self-test (accel or gyro) was ongoing.
    /// I3C TC-sync will be enabled at the end of self-test.
    I3cTcSyncIgnored = 0xE,
    /// Illegal sensor configuration change detected in [`AccelCfg`] or [`GyroCfg`]
    /// register while I3C TC-sync was active.
    I3cIllegalSensorConfig = 0xF,
}

impl FeatureIo1Error {
    pub const fn from_u8(value: u8) -> Self {
        match value {
            0x0 => Self::Active,
            0x1 => Self::Activated,
            0x3 => Self::ConfigDwnldFailed,
            0x5 => Self::NoError,
            0x6 => Self::AxisMapFailed,
            0x8 => Self::I3cTcSyncError,
            0x9 => Self::GyroSelfAborted,
            0xA => Self::GyroSelfIgnored,
            0xB => Self::AccelSelfAborted,
            0xC => Self::AccelSelfIgnored,
            0xD => Self::AutoModeChange,
            0xE => Self::I3cTcSyncIgnored,
            0xF => Self::I3cIllegalSensorConfig,
            _ => Self::NoError,
        }
    }
}

#[repr(u8)]
#[derive(Debug, PartialEq, Eq, Ord, PartialOrd)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Current state of the system
pub(crate) enum FeatureIo1State {
    /// System in feature mode
    FeatureMode = 0x0,
    /// System is executing self-calibration of gyroscope in feature mode
    GyroSelfCalib = 0x1,
    /// System is in self-test mode
    SelfTest = 0x2,
    /// System is in error mode
    Error = 0x3,
}

impl FeatureIo1State {
    pub const fn from_u8(value: u8) -> Self {
        match value {
            0x0 => Self::FeatureMode,
            0x1 => Self::GyroSelfCalib,
            0x2 => Self::SelfTest,
            0x3 => Self::Error,
            _ => Self::Error,
        }
    }
}

/// Feature engine I/O 0 register, address 0x11
#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct FeatEngIo0 {
    #[bits(4, access=RO, from = FeatureIo1Error::from_u8)]
    /// Current feature engine state
    pub errors: FeatureIo1Error,
    #[bits(1, access=RO)]
    /// Self-calibration (gyro only) or self-test (accel or gyro) has been completed.
    /// 0 indicates the procedure is on-going.
    /// 1 indicates the procedure has been completed.
    pub self_proc_done: bool,
    #[bits(1, access=RO)]
    /// Gyroscope self-calibration passed.
    /// [`FeatEngIo0::self_proc_done`] must be '1' for this flag to be valid.
    pub gyro_selfcal_pass: bool,
    #[bits(1, access=RO)]
    /// Gyroscope and/or Accelerometer self-test passed.
    /// [`FeatEngIo0::self_proc_done`] must be '1' for this flag to be valid.
    pub accelgyro_selftest_pass: bool,
    #[bits(1, access=RO)]
    /// Insufficient sample rate for either 50Hz or 200Hz or I3C TC-sync mode.
    pub samp_rate_insufficient: bool,
    #[bits(2)]
    _rsvd1: u8,
    #[bits(1, access=RO)]
    /// Axis remap command done
    pub axis_map_done: bool,
    #[bits(2, access=RO, from = FeatureIo1State::from_u8)]
    /// Current state of the system
    pub state: FeatureIo1State,
    #[bits(3)]
    _rsvd2: u8,
}

impl_register!(FeatEngIo0, 0x11);

/// Feature engine I/O 1 register, address 0x12
/// Before feature engine enable: Feature engine start-up configuration 1 (16 bits)
/// After feature engine enable: Step counter value (low 16 bits)
#[allow(unused)]
pub(crate) const FEATURE_ENG_IO1_ADDR: u8 = 0x12;
/// Feature engine I/O 2 register, address 0x13
/// Before feature engine enable: Feature engine start-up configuration 2 (16 bits)
/// After feature engine enable: Step counter value (high 16 bits)
#[allow(unused)]
pub(crate) const FEATURE_ENG_IO2_ADDR: u8 = 0x13;

#[bitfield(u16)]
/// Feature I/O Synchronization Status and Trigger, address 0x14
pub(crate) struct FeatEngIoStat {
    #[bits(1, access=RW)]
    /// On read: Data has been written by the feature engine
    /// On write: Data written by the host will be sent to the feature engine
    pub io_status: bool,
    #[bits(15)]
    _rsvd: u16,
}

impl_register!(FeatEngIoStat, 0x14);

#[bitfield(u16)]
/// Fifo fill level register, address 0x15
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct FifoFillLevel {
    /// Number of bytes currently stored in the FIFO.
    #[bits(11, access=RO)]
    pub fill_level: u16,
    #[bits(5, default = 0)]
    _rsvd: u8,
}

impl_register!(FifoFillLevel, 0x15);

/// FIFO data register, address 0x16
///
/// IFO read data (16 bits) Data format depends on the setting of
/// register FIFO_CONF. The FIFO data are organized in frames.
/// The new data ﬂag is preserved. Read burst access
/// must be used, the address will not increment when the read burst
/// reads at the address of FIFO_DATA. When a frame is only
/// partially read out it is retransmitted including the
/// header at the next readout.
#[allow(unused)]
pub(crate) const FIFO_DATA_ADDR: u8 = 0x16;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Accelerometer output data rate
pub enum OutputDataRate {
    /// 0.78125 Hz
    Hz0_78125 = 0x1,
    /// 1.5625 Hz
    Hz1_5625 = 0x2,
    /// 3.125 Hz
    Hz3_125 = 0x3,
    /// 6.25 Hz
    Hz6_25 = 0x4,
    /// 12.5 Hz
    Hz12_5 = 0x5,
    /// 25 Hz
    Hz25 = 0x6,
    /// 50 Hz
    Hz50 = 0x7,
    /// 100 Hz
    #[default]
    Hz100 = 0x8,
    /// 200 Hz
    Hz200 = 0x9,
    /// 400 Hz
    Hz400 = 0xA,
    /// 800 Hz
    Hz800 = 0xB,
    /// 1600 Hz
    Hz1600 = 0xC,
    /// 3200 Hz
    Hz3200 = 0xD,
    /// 6400 Hz
    Hz6400 = 0xE,
}

impl OutputDataRate {
    pub(crate) const fn into_bits(self) -> u8 {
        self as u8
    }

    pub(crate) const fn from_u8(value: u8) -> Self {
        match value {
            0x1 => Self::Hz0_78125,
            0x2 => Self::Hz1_5625,
            0x3 => Self::Hz3_125,
            0x4 => Self::Hz6_25,
            0x5 => Self::Hz12_5,
            0x6 => Self::Hz25,
            0x7 => Self::Hz50,
            0x8 => Self::Hz100,
            0x9 => Self::Hz200,
            0xA => Self::Hz400,
            0xB => Self::Hz800,
            0xC => Self::Hz1600,
            0xD => Self::Hz3200,
            0xE => Self::Hz6400,
            _ => Self::Hz100, // Default to 100 Hz
        }
    }

    /// Output data rate in Hz
    pub const fn odr_hz(&self) -> f32 {
        match self {
            Self::Hz0_78125 => 0.78125,
            Self::Hz1_5625 => 1.5625,
            Self::Hz3_125 => 3.125,
            Self::Hz6_25 => 6.25,
            Self::Hz12_5 => 12.5,
            Self::Hz25 => 25.0,
            Self::Hz50 => 50.0,
            Self::Hz100 => 100.0,
            Self::Hz200 => 200.0,
            Self::Hz400 => 400.0,
            Self::Hz800 => 800.0,
            Self::Hz1600 => 1600.0,
            Self::Hz3200 => 3200.0,
            Self::Hz6400 => 6400.0,
        }
    }

    /// Sample delay in microseconds
    pub const fn delay(&self) -> u32 {
        match self {
            Self::Hz0_78125 => 1_280_000,
            Self::Hz1_5625 => 640_000,
            Self::Hz3_125 => 320_000,
            Self::Hz6_25 => 160_000,
            Self::Hz12_5 => 80_000,
            Self::Hz25 => 40_000,
            Self::Hz50 => 20_000,
            Self::Hz100 => 10_000,
            Self::Hz200 => 5_000,
            Self::Hz400 => 2_500,
            Self::Hz800 => 1_250,
            Self::Hz1600 => 625,
            Self::Hz3200 => 313,
            Self::Hz6400 => 156,
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Accelerometer measurement range (raw register value)
pub enum RawAccelRange {
    /// +/- 2g
    G2 = 0x0,
    /// +/- 4g
    G4 = 0x1,
    /// +/- 8g
    #[default]
    G8 = 0x2,
    /// +/- 16g
    G16 = 0x3,
}

impl RawAccelRange {
    pub(crate) const fn into_bits(self) -> u8 {
        self as u8
    }

    pub(crate) const fn from_u8(value: u8) -> Self {
        match value {
            0x0 => Self::G2,
            0x1 => Self::G4,
            0x2 => Self::G8,
            0x3 => Self::G16,
            _ => Self::G2, // Default to +/- 2g
        }
    }

    /// Full scale range in g
    pub const fn range_g(&self) -> f32 {
        match self {
            Self::G2 => 2.0,
            Self::G4 => 4.0,
            Self::G8 => 8.0,
            Self::G16 => 16.0,
        }
    }

    /// Sensitivity in LSB/g
    pub const fn sensitivity(&self) -> f32 {
        match self {
            Self::G2 => 16384.0,
            Self::G4 => 8192.0,
            Self::G8 => 4096.0,
            Self::G16 => 2048.0,
        }
    }

    /// Step up to the next wider range. Returns `None` if already at maximum.
    pub const fn step_up(self) -> Option<Self> {
        match self {
            Self::G2 => Some(Self::G4),
            Self::G4 => Some(Self::G8),
            Self::G8 => Some(Self::G16),
            Self::G16 => None,
        }
    }

    /// Step down to the next narrower range. Returns `None` if already at minimum.
    pub const fn step_down(self) -> Option<Self> {
        match self {
            Self::G2 => None,
            Self::G4 => Some(Self::G2),
            Self::G8 => Some(Self::G4),
            Self::G16 => Some(Self::G8),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Accelerometer measurement range
pub enum AccelRange {
    /// +/- 2g
    G2,
    /// +/- 4g
    G4,
    /// +/- 8g
    #[default]
    G8,
    /// +/- 16g
    G16,
    /// Automatic range selection managed by the driver
    Auto,
}

impl AccelRange {
    /// Convert to the raw register range. `Auto` maps to [`RawAccelRange::G16`] as the initial
    /// widest range before the driver performs automatic range adjustment.
    pub const fn to_raw(self) -> RawAccelRange {
        match self {
            Self::G2 => RawAccelRange::G2,
            Self::G4 => RawAccelRange::G4,
            Self::G8 => RawAccelRange::G8,
            Self::G16 => RawAccelRange::G16,
            Self::Auto => RawAccelRange::G16,
        }
    }
}

impl From<RawAccelRange> for AccelRange {
    fn from(raw: RawAccelRange) -> Self {
        match raw {
            RawAccelRange::G2 => Self::G2,
            RawAccelRange::G4 => Self::G4,
            RawAccelRange::G8 => Self::G8,
            RawAccelRange::G16 => Self::G16,
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Accelerometer measurement bandwidth
pub enum Bandwidth {
    #[default]
    /// ODR/2
    OdrDiv2 = 0x0,
    /// ODR/4
    OdrDiv4 = 0x1,
}

impl Bandwidth {
    pub(crate) const fn into_bits(self) -> u8 {
        self as u8
    }
    pub(crate) const fn from_u8(value: u8) -> Self {
        match value {
            0x0 => Self::OdrDiv2,
            0x1 => Self::OdrDiv4,
            _ => Self::OdrDiv2, // Default to ODR/2
        }
    }

    /// Bandwidth in Hz given the output data rate
    pub const fn bandwidth_hz(&self, odr: &OutputDataRate) -> f32 {
        match self {
            Self::OdrDiv2 => odr.odr_hz() / 2.0,
            Self::OdrDiv4 => odr.odr_hz() / 4.0,
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Accelerometer averaging samples
pub enum AveragingSamples {
    /// 1 sample
    #[default]
    Samples1 = 0x0,
    /// 2 samples
    Samples2 = 0x1,
    /// 4 samples
    Samples4 = 0x2,
    /// 8 samples
    Samples8 = 0x3,
    /// 16 samples
    Samples16 = 0x4,
    /// 32 samples
    Samples32 = 0x5,
    /// 64 samples
    Samples64 = 0x6,
}

impl AveragingSamples {
    pub(crate) const fn into_bits(self) -> u8 {
        self as u8
    }
    pub(crate) const fn from_u8(value: u8) -> Self {
        match value {
            0x0 => Self::Samples1,
            0x1 => Self::Samples2,
            0x2 => Self::Samples4,
            0x3 => Self::Samples8,
            0x4 => Self::Samples16,
            0x5 => Self::Samples32,
            0x6 => Self::Samples64,
            _ => Self::Samples1, // Default to 1 sample
        }
    }

    /// Number of samples averaged
    pub const fn samples(&self) -> u8 {
        1 << (*self as u8)
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Accelerator operation mode
pub enum AccelMode {
    /// Disabled
    #[default]
    Off = 0x0,
    /// Low power mode
    LowPower = 0x3,
    /// Continouous mode with reduced current
    ContReduced = 0x4,
    /// High performance mode
    HighPerformance = 0x7,
}

impl AccelMode {
    pub(crate) const fn into_bits(self) -> u8 {
        self as u8
    }
    pub(crate) const fn from_u8(value: u8) -> Self {
        match value {
            0x0 => Self::Off,
            0x1 => Self::LowPower,
            0x2 => Self::ContReduced,
            0x3 => Self::HighPerformance,
            _ => Self::Off, // Default to Off
        }
    }
}

#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Accelerometer configuration register, address 0x20
pub struct AccelConfig {
    #[bits(4, access=RW, from = OutputDataRate::from_u8, default = OutputDataRate::Hz100)]
    /// Accelerometer output data rate
    pub odr: OutputDataRate,
    #[bits(3, access=RW, from = RawAccelRange::from_u8, default = RawAccelRange::G8)]
    /// Accelerometer measurement range
    pub range: RawAccelRange,
    #[bits(1, access=RW, from = Bandwidth::from_u8, default = Bandwidth::OdrDiv2)]
    /// Accelerometer measurement bandwidth
    pub bandwidth: Bandwidth,
    #[bits(3, access=RW, from = AveragingSamples::from_u8, default = AveragingSamples::Samples1)]
    /// Accelerometer averaging samples
    pub avg_samples: AveragingSamples,
    #[bits(1, default = false)]
    _rsvd1: bool,
    #[bits(3, access=RW, from = AccelMode::from_u8, default = AccelMode::Off)]
    /// Accelerator operation mode
    pub mode: AccelMode,
    #[bits(1, default = false)]
    _rsvd: bool,
}

impl_register!(AccelConfig, 0x20);

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Gyroscope measurement range (raw register value)
pub enum RawGyroRange {
    /// +/-125 dps
    Dps125 = 0x0,
    /// +/-250 dps
    Dps250 = 0x1,
    /// +/-500 dps
    Dps500 = 0x2,
    /// +/-1000 dps
    Dps1000 = 0x3,
    /// +/-2000 dps
    #[default]
    Dps2000 = 0x4,
}

impl RawGyroRange {
    pub(crate) const fn into_bits(self) -> u8 {
        self as u8
    }
    pub(crate) const fn from_u8(value: u8) -> Self {
        match value {
            0x0 => Self::Dps125,
            0x1 => Self::Dps250,
            0x2 => Self::Dps500,
            0x3 => Self::Dps1000,
            0x4 => Self::Dps2000,
            _ => Self::Dps125, // Default to +/- 125 dps
        }
    }

    /// Full scale range in dps
    pub const fn range_dps(&self) -> f32 {
        match self {
            Self::Dps125 => 125.0,
            Self::Dps250 => 250.0,
            Self::Dps500 => 500.0,
            Self::Dps1000 => 1000.0,
            Self::Dps2000 => 2000.0,
        }
    }

    /// Sensitivity in LSB/dps
    pub const fn sensitivity(&self) -> f32 {
        match self {
            Self::Dps125 => 262.144,
            Self::Dps250 => 131.072,
            Self::Dps500 => 65.536,
            Self::Dps1000 => 32.768,
            Self::Dps2000 => 16.384,
        }
    }

    /// Step up to the next wider range. Returns `None` if already at maximum.
    pub const fn step_up(self) -> Option<Self> {
        match self {
            Self::Dps125 => Some(Self::Dps250),
            Self::Dps250 => Some(Self::Dps500),
            Self::Dps500 => Some(Self::Dps1000),
            Self::Dps1000 => Some(Self::Dps2000),
            Self::Dps2000 => None,
        }
    }

    /// Step down to the next narrower range. Returns `None` if already at minimum.
    pub const fn step_down(self) -> Option<Self> {
        match self {
            Self::Dps125 => None,
            Self::Dps250 => Some(Self::Dps125),
            Self::Dps500 => Some(Self::Dps250),
            Self::Dps1000 => Some(Self::Dps500),
            Self::Dps2000 => Some(Self::Dps1000),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Gyroscope measurement range
pub enum GyroRange {
    /// +/-125 dps
    Dps125,
    /// +/-250 dps
    Dps250,
    /// +/-500 dps
    Dps500,
    /// +/-1000 dps
    Dps1000,
    /// +/-2000 dps
    #[default]
    Dps2000,
    /// Automatic range selection managed by the driver
    Auto,
}

impl GyroRange {
    /// Convert to the raw register range. `Auto` maps to [`RawGyroRange::Dps2000`] as the initial
    /// widest range before the driver performs automatic range adjustment.
    pub const fn to_raw(self) -> RawGyroRange {
        match self {
            Self::Dps125 => RawGyroRange::Dps125,
            Self::Dps250 => RawGyroRange::Dps250,
            Self::Dps500 => RawGyroRange::Dps500,
            Self::Dps1000 => RawGyroRange::Dps1000,
            Self::Dps2000 => RawGyroRange::Dps2000,
            Self::Auto => RawGyroRange::Dps2000,
        }
    }
}

impl From<RawGyroRange> for GyroRange {
    fn from(raw: RawGyroRange) -> Self {
        match raw {
            RawGyroRange::Dps125 => Self::Dps125,
            RawGyroRange::Dps250 => Self::Dps250,
            RawGyroRange::Dps500 => Self::Dps500,
            RawGyroRange::Dps1000 => Self::Dps1000,
            RawGyroRange::Dps2000 => Self::Dps2000,
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Gyroscope operation mode
pub enum GyroMode {
    /// Disabled
    #[default]
    Off = 0x0,
    /// Disabled, but gyroscope drive enabled
    DisabledSpun = 0x1,
    /// Enabled with duty cycling
    LowPower = 0x3,
    /// Continouous mode with reduced current
    ContReduced = 0x4,
    /// High performance mode
    HighPerformance = 0x7,
}

impl GyroMode {
    pub(crate) const fn into_bits(self) -> u8 {
        self as u8
    }
    pub(crate) const fn from_u8(value: u8) -> Self {
        match value {
            0x0 => Self::Off,
            0x1 => Self::DisabledSpun,
            0x3 => Self::LowPower,
            0x4 => Self::ContReduced,
            0x7 => Self::HighPerformance,
            _ => Self::Off, // Default to Off
        }
    }
}

#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Gyroscope configuration register, address 0x21
pub struct GyroConfig {
    #[bits(4, access=RW, from = OutputDataRate::from_u8, default = OutputDataRate::Hz100)]
    /// Gyroscope output data rate
    pub odr: OutputDataRate,
    #[bits(3, access=RW, from = RawGyroRange::from_u8, default = RawGyroRange::Dps2000)]
    /// Gyroscope measurement range
    pub range: RawGyroRange,
    #[bits(1, access=RW, from = Bandwidth::from_u8, default = Bandwidth::OdrDiv2)]
    /// Gyroscope measurement bandwidth
    pub bandwidth: Bandwidth,
    #[bits(3, access=RW, from = AveragingSamples::from_u8, default = AveragingSamples::Samples1)]
    /// Gyroscope averaging samples
    pub avg_samples: AveragingSamples,
    #[bits(1, default = false)]
    _rsvd1: bool,
    #[bits(3, access=RW, from = GyroMode::from_u8, default = GyroMode::Off)]
    /// Gyroscope operation mode
    pub mode: GyroMode,
    #[bits(1, default = false)]
    _rsvd: bool,
}

impl_register!(GyroConfig, 0x21);

// TODO: Alternate accelerometer (0x28) and gyroscope (0x29) configuration register
// TODO: Alternate accelerometer and gyroscope configuration register (0x2A)
// TODO: Alternate settings active register (0x2B)

#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// FIFO watermark level register, address 0x35
pub struct FifoWatermark {
    /// FIFO watermark level in bytes. Valid range is 0 to 2047 bytes.
    #[bits(11, access=RW, default=0)]
    pub level: u16,
    #[bits(5, default = 0)]
    _rsvd: u8,
}

impl_register!(FifoWatermark, 0x35);

#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// FIFO configuration register, address 0x36
pub struct FifoConfig {
    #[bits(1, access=RW, default = false)]
    /// - false: Continue writing to the data buffer by overwriting oldest samples
    /// - true: Stop writing to the data buffer when full
    pub stop_on_full: bool,
    #[bits(7, default = 0)]
    _rsvd1: u8,
    #[bits(1, access=RW, default = false)]
    /// FIFO timestamp enable
    /// - false: Do not write timestamp to FIFO
    /// - true: Write timestamp to FIFO
    pub timestamp_en: bool,
    #[bits(1, access=RW, default = false)]
    /// FIFO accelerometer data enable
    /// - false: Do not write accelerometer data to FIFO
    /// - true: Write accelerometer data to FIFO
    pub acc_en: bool,
    #[bits(1, access=RW, default = false)]
    /// FIFO gyroscope data enable
    /// - false: Do not write gyroscope data to FIFO
    /// - true: Write gyroscope data to FIFO
    pub gyr_en: bool,
    #[bits(1, access=RW, default = false)]
    /// FIFO temperature data enable
    /// - false: Do not write temperature data to FIFO
    /// - true: Write temperature data to FIFO
    pub temp_en: bool,
    #[bits(4, default = 0)]
    _rsvd2: u8,
}

impl_register!(FifoConfig, 0x36);

#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// FIFO data buffer control register, address 0x37
pub(crate) struct FifoCtrl {
    #[bits(1, access=RW, default = false)]
    /// FIFO data buffer flush
    /// - false: No action
    /// - true: Flush FIFO data buffer
    pub flush: bool,
    #[bits(15, default = 0)]
    _rsvd: u16,
}

impl_register!(FifoCtrl, 0x37);

#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Interrupt pin configuration register, address 0x38
pub struct IntPinConfig {
    #[bits(1, access=RW, default = false)]
    /// INT1 pin level
    /// - false: Active low
    /// - true: Active high
    pub int1_active_high: bool,
    #[bits(1, access=RW, default = false)]
    /// INT1 pin output type
    /// - false: Push-pull
    /// - true: Open-drain
    pub int1_open_drain: bool,
    #[bits(1, access=RW, default = false)]
    /// INT1 pin output enable
    #[bits(1, access=RW, default = false)]
    pub int1_output_en: bool,
    #[bits(5, default = 0)]
    _rsvd1: u8,
    #[bits(1, access=RW, default = false)]
    /// INT2 pin level
    /// - false: Active low
    /// - true: Active high
    pub int2_active_high: bool,
    #[bits(1, access=RW, default = false)]
    /// INT2 pin output type
    /// - false: Push-pull
    /// - true: Open-drain
    pub int2_open_drain: bool,
    #[bits(1, access=RW, default = false)]
    /// INT2 pin output enable
    pub int2_output_en: bool,
    #[bits(5, default = 0)]
    _rsvd2: u8,
}

impl_register!(IntPinConfig, 0x38);

#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Interrupt latch configuration register, address 0x39
pub struct IntLatchConfig {
    #[bits(1, access=RW, default = false)]
    /// Interrupt permanent latch enable
    /// - false: Interrupts are pulsed
    /// - true: Interrupts are latched until the corresponding interrupt status register is read
    pub int_permanent_latch: bool,
    #[bits(15, default = 0)]
    _rsvd: u16,
}

impl_register!(IntLatchConfig, 0x39);

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Interrupt mapping
pub enum IrqMap {
    /// No interrupt
    #[default]
    None = 0x0,
    /// Map to INT1 pin
    Int1 = 0x1,
    /// Map to INT2 pin
    Int2 = 0x2,
    /// Map to I3C IBI
    Ibi = 0x3,
}

impl IrqMap {
    pub(crate) const fn into_bits(self) -> u8 {
        self as u8
    }
    pub(crate) const fn from_u8(value: u8) -> Self {
        match value {
            0x0 => Self::None,
            0x1 => Self::Int1,
            0x2 => Self::Int2,
            0x3 => Self::Ibi,
            _ => Self::None, // Default to None
        }
    }
}

#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Feature Interrupt mapping register, address 0x3A
pub struct FeatureInterruptMap {
    #[bits(2, access=RW, from = IrqMap::from_u8, default = IrqMap::None)]
    /// No motion interrupt mapping
    pub no_motion: IrqMap,
    #[bits(2, access=RW, from = IrqMap::from_u8, default = IrqMap::None)]
    /// Any motion interrupt mapping
    pub any_motion: IrqMap,
    #[bits(2, access=RW, from = IrqMap::from_u8, default = IrqMap::None)]
    /// Flat detection interrupt mapping
    pub flat: IrqMap,
    #[bits(2, access=RW, from = IrqMap::from_u8, default = IrqMap::None)]
    /// Orientation interrupt mapping
    pub orientation: IrqMap,
    #[bits(2, access=RW, from = IrqMap::from_u8, default = IrqMap::None)]
    /// Step detector interrupt mapping
    pub step_det: IrqMap,
    #[bits(2, access=RW, from = IrqMap::from_u8, default = IrqMap::None)]
    /// Step counter interrupt mapping
    pub step_count: IrqMap,
    #[bits(2, access=RW, from = IrqMap::from_u8, default = IrqMap::None)]
    /// Significant motion interrupt mapping
    pub sig_motion: IrqMap,
    #[bits(2, access=RW, from = IrqMap::from_u8, default = IrqMap::None)]
    /// Tilt interrupt mapping
    pub tilt: IrqMap,
}

impl_register!(FeatureInterruptMap, 0x3A);

#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Sensor interrupt mapping register, address 0x3B
pub struct SensorInterruptMap {
    #[bits(2, access=RW, from = IrqMap::from_u8, default = IrqMap::None)]
    /// Map tap output to IRQ pin or IBI
    pub tap_out: IrqMap,
    #[bits(2, access=RW, from = IrqMap::from_u8, default = IrqMap::None)]
    /// Map I3C output to either IRQ pin or IBI
    pub i3c_out: IrqMap,
    #[bits(2, access=RW, from = IrqMap::from_u8, default = IrqMap::None)]
    /// Map error status output to either IRQ pin or IBI
    pub feat_eng_err: IrqMap,
    #[bits(2, access=RW, from = IrqMap::from_u8, default = IrqMap::None)]
    /// Map temperature data ready output to either IRQ pin or IBI
    pub temp_drdy: IrqMap,
    #[bits(2, access=RW, from = IrqMap::from_u8, default = IrqMap::None)]
    /// Map gyroscope data ready output to either IRQ pin or IBI
    pub gyr_drdy: IrqMap,
    #[bits(2, access=RW, from = IrqMap::from_u8, default = IrqMap::None)]
    /// Map accelerometer data ready output to either IRQ pin or IBI
    pub acc_drdy: IrqMap,
    #[bits(2, access=RW, from = IrqMap::from_u8, default = IrqMap::None)]
    /// Map FIFO watermark output to either IRQ pin or IBI
    pub fifo_watermark: IrqMap,
    #[bits(2, access=RW, from = IrqMap::from_u8, default = IrqMap::None)]
    /// Map FIFO full output to either IRQ pin or IBI
    pub fifo_full: IrqMap,
}

impl_register!(SensorInterruptMap, 0x3B);

#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Feature engine control register, address 0x40
pub struct FeatureEngineControl {
    #[bits(1, access=RW, default = false)]
    /// Enable or disable the feature engine.
    /// Note: A soft-reset is required to re-enable the feature engine.
    pub enable: bool,
    #[bits(15, default = 0)]
    _rsvd: u16,
}

impl_register!(FeatureEngineControl, 0x40);

#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Address register for feature engine configurations and extended output
pub struct FeatEngAddr {
    /// Address for feature engine configurations and extended output
    #[bits(11, access=RW, default = 0)]
    pub addr: u16,
    #[bits(5, default = 0)]
    _rsvd: u8,
}

impl_register!(FeatEngAddr, 0x41);

// #[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd)]
// #[cfg_attr(feature = "defmt", derive(defmt::Format))]
// /// I/O port for data exchange with the feature engine, address 0x42
// pub struct FeatureDataTx(u16);

// impl_register!(0x42, FeatureDataTx);

#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Feature data status register, address 0x43
pub struct FeatureDataStatus {
    #[bits(1, access=RO, default = false)]
    /// Too much data read from or written to the feature engine I/O port.
    /// Bit will be reset upon the next data transfer.
    pub outofbound: bool,
    #[bits(1, access=RO, default = false)]
    /// Data is writeable to the feature engine, or readable from the feature engine.
    pub data_ready: bool,
    #[bits(14, default = 0)]
    _rsvd: u16,
}

impl_register!(FeatureDataStatus, 0x43);

#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Feature engine status register, address 0x45
pub struct FeatureEngineStatus {
    #[bits(1, access=RO, default = false)]
    /// Feature engine halted or sleeping
    pub sleeping: bool,
    #[bits(1, access=RO, default = false)]
    /// Feature engine is transferring data to or from the host
    pub overload: bool,
    #[bits(1, default = false)]
    _rsvd1: bool,
    #[bits(1, access=RO, default = false)]
    /// Feature engine DMA transaction is in progress
    pub dma_ongoing: bool,
    #[bits(1, access=RO, default = false)]
    /// Feature engine was disabled by the host
    pub disabled: bool,
    #[bits(1, access=RO, default = false)]
    /// Feature engine did not acknowledge internal watchdog. Perform
    /// a soft-reset to re-enable the feature engine.
    pub wdt_error: bool,
    #[bits(10, default = 0)]
    _rsvd: u16,
}

impl_register!(FeatureEngineStatus, 0x45);

#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// I/O Pad Strength configuration register, address 0x51
pub struct IoPadStrength {
    #[bits(3, access=RW, default = 0)]
    /// Generic drive strength control for the output pads. 0 to 7 steps.
    pub drive_strength: u8,
    #[bits(1, access=RW, default = false)]
    /// Enable drive strength for SCL and SDA pins when interfacing via I2C.
    pub i2c_boost: bool,
    #[bits(12, default = 0)]
    _rsvd: u16,
}

impl_register!(IoPadStrength, 0x51);

#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// I2C interface-specific configuration register, address 0x52
pub struct I2cWatchdogConfig {
    #[bits(1, access=RW, default = false)]
    /// I2C watchdog timer period selection.
    /// - false: 1.25 ms
    /// - true: 40 ms
    pub timeout: bool,
    #[bits(1, access=RW, default = false)]
    /// I2C watchdog enable
    pub enable: bool,
    #[bits(14, default = 0)]
    _rsvd: u16,
}

impl_register!(I2cWatchdogConfig, 0x52);

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Command register, address 0x7E
pub(crate) enum Command {
    /// No operation
    Nop = 0x0,
    /// Trigger the self-test of the device. Default scope of
    /// the self-test is a test of the accelerometer and gyroscope.
    /// Notes: an enabled feature engine is required; further
    /// settings are possible via the feature engine data interface.
    SelfTest = 0x100,
    /// Trigger the self-calibration of the gyroscope. Notes: an enabled feature engine
    /// is required; further settings are possible via the feature engine data interface.
    GyroSelfCalib = 0x101,
    /// Abort a running self-calibration of the gyroscope.
    GyroSelfCalibAbort = 0x200,
    /// Update the conﬁguration of the I3C timing control synchonronous feature
    /// written to all or any of I3C_TC_SYNC_TPH, I3C_TC_SYNC_TU and
    /// I3C_TC_SYNC_ODR.
    I3cTcSync = 0x201,
    /// Update axis mapping
    AxisRemap = 0x300,
    /// Reset the device. All registers are set to default values.
    SoftReset = 0xDEAF,
}

impl From<u16> for Command {
    fn from(value: u16) -> Self {
        match value {
            0x0 => Self::Nop,
            0x100 => Self::SelfTest,
            0x101 => Self::GyroSelfCalib,
            0x200 => Self::GyroSelfCalibAbort,
            0x201 => Self::I3cTcSync,
            0x300 => Self::AxisRemap,
            0xDEAF => Self::SoftReset,
            _ => Self::Nop, // Default to Nop
        }
    }
}

impl Register for Command {
    const ADDRESS: u8 = 0x7E;
    #[inline(always)]
    fn from_u16(value: u16) -> Self {
        Self::from(value)
    }
    #[inline(always)]
    fn to_u16(&self) -> u16 {
        *self as u16
    }
}

#[bitfield(u16)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Gyroscope self-calibration select, address 0x26
pub struct GyroSelfCalibSelect {
    #[bits(1, access=RW, default = false)]
    /// Enable gyroscope self-calibration of sensitivity
    pub sensitivity: bool,
    #[bits(1, access=RW, default = false)]
    /// Enable gyroscope self-calibration of offset
    pub offset: bool,
    #[bits(1, access=RW, default = false)]
    /// Apply the gyroscope self-calibration corrections
    pub apply: bool,
    #[bits(13, default = 0)]
    _rsvd: u16,
}

impl_register!(GyroSelfCalibSelect, 0x26);
