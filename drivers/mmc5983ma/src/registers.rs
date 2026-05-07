use bitfield_struct::bitfield;

use crate::trace;

use crate::MagMeasurementRaw;

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
}

pub(crate) const MMC5983_DEVICE_ID: u8 = 0x30;

#[bitfield(u8)]
pub(crate) struct XYZOut2 {
    #[bits(2)]
    _rsvd: u8,
    #[bits(2, access = RO)]
    pub zout: u8,
    #[bits(2, access = RO)]
    pub yout: u8,
    #[bits(2, access = RO)]
    pub xout: u8,
}

impl_register!(XYZOut2, 0x06);

#[bitfield(u8)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct StatusRegister {
    /// Magnetic measurement done
    #[bits(1, access = RW)]
    pub magmeas_done: bool,
    /// Temperature measurement done
    #[bits(1, access = RW)]
    pub tmeas_done: bool,
    #[bits(2)]
    _rsvd1: u8,
    /// OTP read done
    #[bits(1, access = RW)]
    pub otp_read_done: bool,
    #[bits(3)]
    _rsvd2: u8,
}

impl_register!(StatusRegister, 0x08);

#[bitfield(u8)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct MeasurementTriggerControl {
    /// Take magnetic field measurement, set ‘1’ will initiate measurement. This bit will be
    /// automatically reset to 0 at the end of each measurement.
    #[bits(access = WO)]
    pub tm_m: bool,
    /// Take Temperature measurement, set ‘1’ will initiate measurement. This bit will be
    /// automatically reset to 0 at the end of each measurement. This bit and TM_M cannot be high
    /// at the same time.
    #[bits(access = WO)]
    pub tm_t: bool,
    /// Writing “1” will enable the interrupt for completed measurements. Once a measurement is
    /// finished, either magnetic field or temperature, an interrupt will be sent to the host.
    #[bits(access = WO)]
    pub drdy: bool,
    /// Writing “1” will cause the chip to do the Set operation, which will allow large set current
    /// to flow through the sensor coils for 500ns. This bit is self-cleared at the end of Set
    /// operation.
    #[bits(access = WO)]
    pub m_set: bool,
    /// Writing “1” will cause the chip to do the Reset operation, which will allow large reset
    /// current to flow through the sensor coils for 500ns. This bit is self-cleared at the end of
    /// Reset operation.
    #[bits(access = WO)]
    pub m_reset: bool,
    /// Writing “1” will enable the feature of automatic set/reset.
    #[bits(access = WO)]
    pub auto_sr_en: bool,
    /// Writing “1” will let the device to read the OTP data again. This bit will be automatically
    /// reset to 0 after the shadow registers for OTP are refreshed
    #[bits(access = WO)]
    pub otp_read: bool,
    _rsvd: bool,
}

impl_register!(MeasurementTriggerControl, 0x09);

#[bitfield(u8)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct AnalogControl {
    /// "Output Resolution"
    /// 00: 8ms (100Hz)
    /// 01: 4ms (200Hz)
    /// 10: 2ms (400Hz)
    /// 11: 1ms (800Hz)
    #[bits(2, access = WO)]
    pub bw: u8,
    /// 1: Disable channel
    /// 0: Enable channel
    #[bits(1, access = WO)]
    pub x_inhibit: bool,
    /// 11: Disable channels
    /// 00: Enable channels
    #[bits(2, access = WO)]
    pub yz_inhibit: u8,
    #[bits(2)]
    _rsvd: u8,
    // Used in `reset_chip`
    #[bits(1, access = WO)]
    pub sw_rst: bool,
}

impl_register!(AnalogControl, 0x0A);

#[bitfield(u8)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct DigitalControl {
    /// These bits determine how often the chip will take measurements in Continuous
    /// Measurement Mode. The frequency is based on the assumption that BW[1:0] = 00.
    ///
    /// 000: Continuous Measurement Mode is off.
    /// 001: 1 Hz
    /// 010: 10 Hz
    /// 011: 20 Hz
    /// 100: 50 Hz
    /// 101: 100 Hz
    /// 110: (BW=01) 200 Hz
    /// 111: (BW=11) 1000 Hz
    #[bits(3, access = WO)]
    pub cm_freq: u8,
    /// 1: Enable Continuous Measurement Mode
    /// 0: Disable Continuous Measurement Mode
    /// CM_FREQ must be set to a non-zero value for this bit to have any effect.
    #[bits(1, access = WO)]
    pub cmm_en: bool,

    /// These bits determine how often the chip will do a set operation. The device will perform a
    /// SET automatically per the setting in below table.
    ///
    /// Prd_set [2:0] Times of measurement
    /// 000: 1
    /// 001: 25
    /// 010: 75
    /// 011: 100
    /// 100: 250
    /// 101: 500
    /// 110: 1000
    /// 111: 2000
    #[bits(3, access = WO)]
    pub prd_set: u8,
    /// Writing “1” will enable the feature of periodic set. This feature needs to work with both
    /// Auto_SR_en and Cmm_en bits set to 1.
    #[bits(1, access = WO)]
    pub en_prd_set: bool,
}

impl_register!(DigitalControl, 0x0B);

#[bitfield(u8)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct IoControl {
    _rsvd0: bool,
    /// Writing “1” will apply an extra current flowing from the positive end to the negative end
    /// of an internal coil and result in an extra magnetic field. This feature can be used to
    /// check whether the sensor has been saturated
    #[bits(1, access = WO)]
    pub st_enp: bool,
    /// Writing “1” will apply an extra current flowing from the negative end to the positive end
    /// of an internal coil and result in an extra magnetic field. This feature can be used to
    /// check whether the sensor has been saturated
    #[bits(1, access = WO)]
    pub st_enm: bool,
    #[bits(3)]
    _rsvd1: u8,
    /// Writing a 1 into this location will put the device into 3-wire SPI mode.
    #[bits(1, access = WO)]
    pub spi_3w_en: bool,
    _rsvd2: bool,
}

impl_register!(IoControl, 0x0C);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProductId(u8);

impl From<u8> for ProductId {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

impl_register!(ProductId, 0x2F);

impl From<[u8; 7]> for MagMeasurementRaw {
    #[inline(always)]
    fn from(value: [u8; 7]) -> Self {
        let xyzr = value[6];
        let x = (value[0] as u32) << 10 | (value[1] as u32) << 2 | ((xyzr >> 6) & 0x3) as u32;
        let y = (value[2] as u32) << 10 | (value[3] as u32) << 2 | ((xyzr >> 4) & 0x3) as u32;
        let z = (value[4] as u32) << 10 | (value[5] as u32) << 2 | ((xyzr >> 2) & 0x3) as u32;

        trace!(
            "X: {}, Y: {}, Z: {}",
            x as i32 - (1 << 17),
            y as i32 - (1 << 17),
            z as i32 - (1 << 17)
        );
        MagMeasurementRaw::new().with_x(x).with_y(y).with_z(z)
    }
}
