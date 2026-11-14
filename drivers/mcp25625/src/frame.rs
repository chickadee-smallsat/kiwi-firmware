//! CAN 2.0B identifiers and data frames.
//!
//! These are defined locally rather than pulled in from a generic HAL crate:
//! the widely used `embedded-can` crate only exposes a non-blocking (`nb`)
//! polling API for actual bus I/O, which is not a fit for an
//! `embassy`-flavored `async` driver. The `Id`/`Frame` shape below is kept
//! deliberately close to that ecosystem's so it stays easy to reason about.

/// Standard 11-bit CAN identifier (`0..=0x7FF`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct StandardId(u16);

impl StandardId {
    /// CAN ID `0`, the highest priority.
    pub const ZERO: Self = Self(0);
    /// CAN ID `0x7FF`, the lowest priority.
    pub const MAX: Self = Self(0x7FF);

    /// Creates a `StandardId` from a raw value, returning `None` if it exceeds 11 bits.
    #[inline]
    pub const fn new(raw: u16) -> Option<Self> {
        if raw <= 0x7FF {
            Some(Self(raw))
        } else {
            None
        }
    }

    /// Returns the raw 11-bit value.
    #[inline]
    pub const fn as_raw(self) -> u16 {
        self.0
    }
}

/// Extended 29-bit CAN identifier (`0..=0x1FFF_FFFF`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ExtendedId(u32);

impl ExtendedId {
    /// CAN ID `0`, the highest priority.
    pub const ZERO: Self = Self(0);
    /// CAN ID `0x1FFF_FFFF`, the lowest priority.
    pub const MAX: Self = Self(0x1FFF_FFFF);

    /// Creates an `ExtendedId` from a raw value, returning `None` if it exceeds 29 bits.
    #[inline]
    pub const fn new(raw: u32) -> Option<Self> {
        if raw <= 0x1FFF_FFFF {
            Some(Self(raw))
        } else {
            None
        }
    }

    /// Returns the raw 29-bit value.
    #[inline]
    pub const fn as_raw(self) -> u32 {
        self.0
    }
}

/// A CAN identifier, either standard (11-bit) or extended (29-bit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Id {
    /// Standard 11-bit identifier.
    Standard(StandardId),
    /// Extended 29-bit identifier.
    Extended(ExtendedId),
}

impl From<StandardId> for Id {
    #[inline]
    fn from(id: StandardId) -> Self {
        Id::Standard(id)
    }
}

impl From<ExtendedId> for Id {
    #[inline]
    fn from(id: ExtendedId) -> Self {
        Id::Extended(id)
    }
}

impl Id {
    /// Returns `true` if this is an extended (29-bit) identifier.
    pub const fn is_extended(&self) -> bool {
        matches!(self, Id::Extended(_))
    }
}

/// Maximum data length of a classic CAN 2.0B frame.
pub const MAX_DATA_LEN: usize = 8;

/// A CAN 2.0B data or remote frame (up to 8 data bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct CanFrame {
    id: Id,
    rtr: bool,
    dlc: u8,
    data: [u8; MAX_DATA_LEN],
}

impl CanFrame {
    /// Creates a new data frame. Returns `None` if `data` is longer than 8 bytes.
    pub fn new(id: impl Into<Id>, data: &[u8]) -> Option<Self> {
        if data.len() > MAX_DATA_LEN {
            return None;
        }
        let mut buf = [0u8; MAX_DATA_LEN];
        buf[..data.len()].copy_from_slice(data);
        Some(Self {
            id: id.into(),
            rtr: false,
            dlc: data.len() as u8,
            data: buf,
        })
    }

    /// Creates a new remote (RTR) frame requesting `dlc` bytes. Returns `None` if `dlc > 8`.
    pub fn new_remote(id: impl Into<Id>, dlc: usize) -> Option<Self> {
        if dlc > MAX_DATA_LEN {
            return None;
        }
        Some(Self {
            id: id.into(),
            rtr: true,
            dlc: dlc as u8,
            data: [0; MAX_DATA_LEN],
        })
    }

    /// Returns the identifier of this frame.
    pub const fn id(&self) -> Id {
        self.id
    }

    /// Returns `true` if this is an extended (29-bit) frame.
    pub const fn is_extended(&self) -> bool {
        self.id.is_extended()
    }

    /// Returns `true` if this is a standard (11-bit) frame.
    pub const fn is_standard(&self) -> bool {
        !self.id.is_extended()
    }

    /// Returns `true` if this is a remote (RTR) frame.
    pub const fn is_remote_frame(&self) -> bool {
        self.rtr
    }

    /// Returns `true` if this is a data frame.
    pub const fn is_data_frame(&self) -> bool {
        !self.rtr
    }

    /// Returns the data length code (0..=8).
    pub const fn dlc(&self) -> usize {
        self.dlc as usize
    }

    /// Returns the frame data. Empty for remote frames.
    pub fn data(&self) -> &[u8] {
        if self.rtr {
            &[]
        } else {
            &self.data[..self.dlc as usize]
        }
    }

    pub(crate) const fn from_raw(id: Id, rtr: bool, dlc: u8, data: [u8; MAX_DATA_LEN]) -> Self {
        Self { id, rtr, dlc, data }
    }
}

/// Encodes an identifier into the 4 ID bytes used by both TX and RX buffers
/// (`SIDH, SIDL, EID8, EID0`), setting the extended-identifier bit as needed.
///
/// Wire layout (bit 7 first in each byte), per the MCP25625/MCP2515 TX/RX
/// buffer registers:
///
/// ```text
/// SIDH:  SID10 SID9 SID8 SID7 SID6 SID5 SID4 SID3
/// SIDL:  SID2  SID1 SID0  --  IDE  --  EID17 EID16
/// EID8:  EID15 EID14 EID13 EID12 EID11 EID10 EID9 EID8
/// EID0:  EID7  EID6  EID5  EID4  EID3  EID2  EID1 EID0
/// ```
///
/// For a standard (11-bit) identifier, `SID[10:0]` is the whole ID, `IDE` is
/// clear, and the `EID`/unused bits are all `0`. For an extended (29-bit)
/// identifier, `SID[10:0]` holds the top 11 bits (`id[28:18]`), `IDE` is set,
/// and `EID[17:0]` holds the remaining 18 bits (`id[17:0]`).
///
/// Note: on RX buffers this same SIDL bit 4 is instead SRR (substitute
/// remote request, meaningful only for extended frames received on RXB0) —
/// this driver ignores it either way, since [`CanFrame`] tracks RTR
/// separately from the DLC byte's own RTR bit.
pub(crate) fn encode_id(id: Id) -> [u8; 4] {
    match id {
        Id::Standard(sid) => {
            let raw = sid.as_raw();
            let sidh = (raw >> 3) as u8;
            let sidl = ((raw & 0x7) as u8) << 5;
            [sidh, sidl, 0, 0]
        }
        Id::Extended(eid) => {
            let raw = eid.as_raw();
            let sid11 = ((raw >> 18) & 0x7FF) as u16;
            let eid_lo18 = raw & 0x3_FFFF;
            let sidh = (sid11 >> 3) as u8;
            let sidl = (((sid11 & 0x7) as u8) << 5) | 0x08 | (((eid_lo18 >> 16) & 0x3) as u8);
            let eid8 = ((eid_lo18 >> 8) & 0xFF) as u8;
            let eid0 = (eid_lo18 & 0xFF) as u8;
            [sidh, sidl, eid8, eid0]
        }
    }
}

/// Decodes the 4 ID bytes (`SIDH, SIDL, EID8, EID0`) of a TX or RX buffer into
/// an [`Id`]. The extended-identifier bit (SIDL bit 3) selects standard vs.
/// extended decoding; see [`encode_id`] for the full byte layout this reverses.
pub(crate) fn decode_id(bytes: [u8; 4]) -> Id {
    let [sidh, sidl, eid8, eid0] = bytes;
    let sid11 = ((sidh as u16) << 3) | ((sidl >> 5) as u16);
    if sidl & 0x08 != 0 {
        let eid_lo18 = (((sidl & 0x03) as u32) << 16) | ((eid8 as u32) << 8) | (eid0 as u32);
        let raw = ((sid11 as u32) << 18) | eid_lo18;
        Id::Extended(ExtendedId::new(raw).unwrap_or(ExtendedId::ZERO))
    } else {
        Id::Standard(StandardId::new(sid11).unwrap_or(StandardId::ZERO))
    }
}
