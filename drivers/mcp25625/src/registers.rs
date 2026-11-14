//! MCP25625 SPI instruction set and register map.
//!
//! Addresses and bit positions are taken from the MCP25625 datasheet
//! (DS20005282), which shares its register map with the MCP2515.
//!
//! `CanIntE`/`CanIntF` share one field layout via the `int_bits!` macro even
//! though each instantiation only uses a subset of the generated accessors
//! (CANINTE only ever needs setters; the driver's receive loop only reads a
//! few of CANINTF's flags explicitly and folds the rest via `into_bits()`),
//! so dead-code warnings on individual accessors are expected here.
#![allow(dead_code)]

use bitfield_struct::bitfield;

// ---------------------------------------------------------------------
// SPI instruction set
// ---------------------------------------------------------------------
//
// Every instruction below is issued as a single `SpiDevice::transaction`
// (one CS assertion). Byte formats are from the MCP25625/MCP2515 datasheet
// section on the SPI interface:

/// `1100_0000`. No operand bytes. Resets internal registers to their default
/// state and forces Configuration mode, equivalent to (but distinct from,
/// see [`crate::driver::Mcp25625::init`]) a hardware reset via the RST pin.
pub(crate) const INSTR_RESET: u8 = 0xC0;
/// `0000_0011 addr` -> reads back one byte from `addr`. The address pointer
/// auto-increments, so a longer read continues into subsequent registers,
/// but this driver only ever reads a single byte at a time through it.
pub(crate) const INSTR_READ: u8 = 0x03;
/// `1001_0nm0`: reads one RX buffer starting at a fixed offset, skipping the
/// need to send an explicit address byte. `n` (bit 2) selects RXB0/RXB1 and
/// `m` (bit 1) selects the start point (`0` = SIDH, `1` = D0); this driver
/// always starts at SIDH (`n << 2`, see [`Bus::read_rx_buffer`]) so it reads
/// the identifier, DLC, and data in one transaction.
pub(crate) const INSTR_READ_RX_BUFFER: u8 = 0x90;
/// `0000_0010 addr value`. Writes one byte to `addr`.
pub(crate) const INSTR_WRITE: u8 = 0x02;
/// `0100_0abc`: loads a TX buffer starting at a fixed offset. Bits 2:1 (`ab`)
/// select TXB0/1/2 and bit 0 (`c`) the start point (`0` = SIDH, `1` = D0);
/// this driver always starts at SIDH (`idx << 1`, see [`Bus::load_tx_buffer`]).
pub(crate) const INSTR_LOAD_TX_BUFFER: u8 = 0x40;
/// `1000_0nnn`: Request-To-Send. Bits 2:0 each independently request
/// transmission of TXB2/1/0; multiple bits may be set in one instruction to
/// queue several buffers at once (this driver always sets exactly one).
pub(crate) const INSTR_RTS: u8 = 0x80;
/// `1010_0000` -> one status byte; see [`Bus::read_status`] for its layout.
/// A quick, single-byte alternative to reading CANINTF/TXBnCTRL individually.
pub(crate) const INSTR_READ_STATUS: u8 = 0xA0;
/// `0000_0101 addr mask value`: for each bit set in `mask`, replaces that bit
/// in the register at `addr` with the corresponding bit of `value`, leaving
/// all other bits untouched. Only usable on the subset of registers the
/// datasheet marks BIT-MODIFY-capable (control/status registers — not TX/RX
/// buffers, filters, or masks).
pub(crate) const INSTR_BIT_MODIFY: u8 = 0x05;

// ---------------------------------------------------------------------
// Control / status registers
// ---------------------------------------------------------------------

/// CANSTAT: actual operating mode + pending-interrupt code (read-only).
pub(crate) const REG_CANSTAT: u8 = 0x0E;
/// CANCTRL: requested operating mode + CLKOUT/one-shot/abort controls.
pub(crate) const REG_CANCTRL: u8 = 0x0F;
/// TEC: transmit error counter (saturates at 255; see [`crate::Mcp25625::error_counters`]).
pub(crate) const REG_TEC: u8 = 0x1C;
/// REC: receive error counter (saturates at 255).
pub(crate) const REG_REC: u8 = 0x1D;
/// CNF3: phase segment 2, wake-up filter, and CLKOUT/SOF selection.
pub(crate) const REG_CNF3: u8 = 0x28;
/// CNF2: propagation segment, phase segment 1, triple-sample, PHSEG2 source select.
pub(crate) const REG_CNF2: u8 = 0x29;
/// CNF1: baud rate prescaler and synchronization jump width.
pub(crate) const REG_CNF1: u8 = 0x2A;
/// CANINTE: per-source interrupt enables (same bit layout as CANINTF).
pub(crate) const REG_CANINTE: u8 = 0x2B;
/// CANINTF: per-source latched interrupt flags (same bit layout as CANINTE).
pub(crate) const REG_CANINTF: u8 = 0x2C;
/// EFLG: latched error/warning flags; see [`Eflg`].
pub(crate) const REG_EFLG: u8 = 0x2D;

/// SIDH address of each of the 6 acceptance filters (RXF0..RXF5).
pub(crate) const REG_RXF_SIDH: [u8; 6] = [0x00, 0x04, 0x08, 0x10, 0x14, 0x18];
/// SIDH address of each of the 2 acceptance masks (RXM0, RXM1).
pub(crate) const REG_RXM_SIDH: [u8; 2] = [0x20, 0x24];

/// CTRL register address of each of the 2 RX buffers. TX buffer selection for
/// the data path itself is encoded directly in the LOAD TX BUFFER / RTS
/// instruction opcodes, so no equivalent table is needed there — but
/// `TXB_CTRL` below is still read for diagnostics (see [`TxbCtrl`]).
pub(crate) const RXB_CTRL: [u8; 2] = [0x60, 0x70];

/// CTRL register address of each of the 3 TX buffers (TXB0CTRL, TXB1CTRL,
/// TXB2CTRL), used only for diagnostics: [`crate::driver::Mcp25625::transmit_impl`]
/// reads these when all 3 buffers are busy, to tell an ordinary arbitration
/// loss/retry apart from an actual bus error (see [`TxbCtrl`]).
pub(crate) const TXB_CTRL: [u8; 3] = [0x30, 0x40, 0x50];

// ---------------------------------------------------------------------
// Bitfield register contents
// ---------------------------------------------------------------------

/// Requested/actual operating mode, CANCTRL.REQOP / CANSTAT.OPMOD.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum OpMode {
    /// Normal operation: transmit and receive, participate in bus arbitration.
    #[default]
    Normal = 0b000,
    /// Sleep: low power, wake-up on bus activity.
    Sleep = 0b001,
    /// Loopback: internally routes TX to RX, no bus activity, for self-test.
    Loopback = 0b010,
    /// Listen-only: receive only, never transmits (not even ACK).
    ListenOnly = 0b011,
    /// Configuration: the only mode in which CNF1-3 and filters/masks may be written.
    Configuration = 0b100,
}

impl OpMode {
    pub(crate) const fn from_bits(bits: u8) -> Self {
        match bits & 0b111 {
            0b000 => Self::Normal,
            0b001 => Self::Sleep,
            0b010 => Self::Loopback,
            0b011 => Self::ListenOnly,
            _ => Self::Configuration,
        }
    }
    pub(crate) const fn into_bits(self) -> u8 {
        self as u8
    }
}

#[bitfield(u8)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// CANCTRL register, address 0x0F.
pub(crate) struct CanCtrl {
    #[bits(2, access = RW)]
    /// CLKOUT pin prescaler.
    pub clkpre: u8,
    #[bits(1, access = RW)]
    /// Enable the CLKOUT pin.
    pub clken: bool,
    #[bits(1, access = RW)]
    /// One-shot mode: do not retransmit on arbitration loss or error.
    pub osm: bool,
    #[bits(1, access = RW)]
    /// Abort all pending transmissions.
    pub abat: bool,
    #[bits(3, access = RW, from = OpMode::from_bits, into = OpMode::into_bits)]
    /// Requested operating mode.
    pub reqop: OpMode,
}

#[bitfield(u8)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// CANSTAT register, address 0x0E.
pub(crate) struct CanStat {
    #[bits(1, default = false)]
    _rsvd0: bool,
    #[bits(3, access = RO)]
    /// Index of the highest-priority pending interrupt.
    pub icod: u8,
    #[bits(1, default = false)]
    _rsvd4: bool,
    #[bits(3, access = RO, from = OpMode::from_bits, into = OpMode::into_bits)]
    /// Actual operating mode the device is in.
    pub opmod: OpMode,
}

#[bitfield(u8)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// CNF1 register, address 0x2A.
pub(crate) struct Cnf1 {
    #[bits(6, access = RW)]
    /// Baud rate prescaler: TQ = 2 * (BRP + 1) / Fosc.
    pub brp: u8,
    #[bits(2, access = RW)]
    /// Synchronization jump width, in TQ, minus one.
    pub sjw: u8,
}

#[bitfield(u8)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// CNF2 register, address 0x29.
pub(crate) struct Cnf2 {
    #[bits(3, access = RW)]
    /// Propagation segment length, in TQ, minus one.
    pub prseg: u8,
    #[bits(3, access = RW)]
    /// Phase segment 1 length, in TQ, minus one.
    pub phseg1: u8,
    #[bits(1, access = RW)]
    /// Sample every bit three times instead of once.
    pub sam: bool,
    #[bits(1, access = RW)]
    /// Phase segment 2 length is set explicitly by CNF3.PHSEG2 (must be set).
    pub btlmode: bool,
}

#[bitfield(u8)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// CNF3 register, address 0x28.
pub(crate) struct Cnf3 {
    #[bits(3, access = RW)]
    /// Phase segment 2 length, in TQ, minus one.
    pub phseg2: u8,
    #[bits(3, default = 0)]
    _rsvd: u8,
    #[bits(1, access = RW)]
    /// Filter wake-up frames shorter than one bit time.
    pub wakfil: bool,
    #[bits(1, access = RW)]
    /// Drive CLKOUT with the Start-of-Frame signal instead (if CLKEN=0).
    pub sof: bool,
}

/// Shared bit layout of CANINTE (enables) and CANINTF (latched flags): the
/// two registers put the same 8 interrupt sources at the same bit positions,
/// so one field layout serves both. Field doc comments below are phrased in
/// terms of the flag (CANINTF) meaning; on CANINTE the same bit instead
/// enables that source's contribution to the INT pin.
macro_rules! int_bits {
    ($name:ident, $doc:literal) => {
        #[bitfield(u8)]
        #[cfg_attr(feature = "defmt", derive(defmt::Format))]
        #[doc = $doc]
        pub(crate) struct $name {
            #[bits(1, access = RW)]
            /// RXB0 full: a message is waiting in RXB0 (cleared by reading it, or explicitly via BIT MODIFY).
            pub rx0: bool,
            #[bits(1, access = RW)]
            /// RXB1 full: a message is waiting in RXB1.
            pub rx1: bool,
            #[bits(1, access = RW)]
            /// TXB0 empty: the last transmission from TXB0 completed (successfully or not).
            pub tx0: bool,
            #[bits(1, access = RW)]
            /// TXB1 empty: the last transmission from TXB1 completed.
            pub tx1: bool,
            #[bits(1, access = RW)]
            /// TXB2 empty: the last transmission from TXB2 completed.
            pub tx2: bool,
            #[bits(1, access = RW)]
            /// Error interrupt: set when EFLG changes (any warning/passive/bus-off
            /// transition), or on RXB0/RXB1 overflow. See [`Eflg`] for the details.
            pub err: bool,
            #[bits(1, access = RW)]
            /// Wake-up: bus activity was detected while in Sleep mode.
            pub wake: bool,
            #[bits(1, access = RW)]
            /// Message error: a bus error (form/CRC/stuff/ACK/bit) occurred.
            pub merr: bool,
        }
    };
}

int_bits!(
    CanIntE,
    "CANINTE register, address 0x2B: interrupt enables."
);
int_bits!(CanIntF, "CANINTF register, address 0x2C: interrupt flags (RW, but bits only ever self-clear via BIT MODIFY).");

#[bitfield(u8)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// EFLG register, address 0x2D: error flags.
pub struct Eflg {
    #[bits(1, access = RW)]
    /// Error warning: TEC or REC >= 96.
    pub ewarn: bool,
    #[bits(1, access = RW)]
    /// Receive error warning: REC >= 96.
    pub rxwar: bool,
    #[bits(1, access = RW)]
    /// Transmit error warning: TEC >= 96.
    pub txwar: bool,
    #[bits(1, access = RW)]
    /// Receive error passive: REC >= 128.
    pub rxep: bool,
    #[bits(1, access = RW)]
    /// Transmit error passive: TEC >= 128.
    pub txep: bool,
    #[bits(1, access = RW)]
    /// Bus-off: TEC >= 256.
    pub txbo: bool,
    #[bits(1, access = RW)]
    /// RXB0 overflow: a message was lost because RXB0 was full.
    pub rx0ovr: bool,
    #[bits(1, access = RW)]
    /// RXB1 overflow: a message was lost because RXB1 was full.
    pub rx1ovr: bool,
}

#[bitfield(u8)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// TXBnCTRL register. TX completion itself is inferred from CANINTF/READ
/// STATUS, not this register, but [`crate::driver::Mcp25625::transmit_impl`]
/// reads it as a diagnostic when all 3 TX buffers are found busy, to
/// distinguish ordinary arbitration loss (`mloa`, expected on a live bus with
/// more than one active transmitter — the retry should still eventually
/// succeed) from an actual bus error (`txerr`: no ACK, or a CRC/form/stuff
/// error, which usually means wiring, termination, or a bit-timing mismatch).
pub(crate) struct TxbCtrl {
    #[bits(2, access = RW)]
    /// Transmit buffer priority (0 = lowest, 3 = highest).
    pub txp: u8,
    #[bits(1, default = false)]
    _rsvd2: bool,
    #[bits(1, access = RW)]
    /// Request transmission of this buffer; cleared by the device once sent
    /// (successfully or, absent auto-retransmit, after an error/arbitration loss).
    pub txreq: bool,
    #[bits(1, access = RO)]
    /// Set if a bus error (form, CRC, stuff, or ACK error — not arbitration
    /// loss, see [`Self::mloa`]) occurred during the last transmission attempt.
    pub txerr: bool,
    #[bits(1, access = RO)]
    /// Set if the last transmission attempt lost arbitration to another node.
    pub mloa: bool,
    #[bits(1, access = RO)]
    /// Set if the last transmission was aborted, either because ABAT was set
    /// while it was pending, or (in one-shot mode) after a single failed attempt.
    pub abtf: bool,
    #[bits(1, default = false)]
    _rsvd7: bool,
}

#[bitfield(u8)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// RXB0CTRL register.
pub(crate) struct Rxb0Ctrl {
    #[bits(1, access = RO)]
    /// Set if a matching message arrived while RXB0 was already full (rolled over to RXB1 if BUKT is set).
    pub filhit0: bool,
    #[bits(1, access = RO)]
    /// Copy of BUKT for reference.
    pub bukt1: bool,
    #[bits(1, access = RW)]
    /// Roll over to RXB1 on RXB0 overflow instead of dropping the message.
    pub bukt: bool,
    #[bits(1, access = RO)]
    /// Set if the received message was a remote request.
    pub rxrtr: bool,
    #[bits(1, default = false)]
    _rsvd4: bool,
    #[bits(2, access = RW)]
    /// Receive mode: 0b11 disables filtering and accepts every message.
    pub rxm: u8,
    #[bits(1, default = false)]
    _rsvd7: bool,
}

#[bitfield(u8)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// RXB1CTRL register.
pub(crate) struct Rxb1Ctrl {
    #[bits(3, access = RO)]
    /// Index of the filter (RXF2..RXF5) that matched.
    pub filhit: u8,
    #[bits(1, access = RO)]
    /// Set if the received message was a remote request.
    pub rxrtr: bool,
    #[bits(1, default = false)]
    _rsvd4: bool,
    #[bits(2, access = RW)]
    /// Receive mode: 0b11 disables filtering and accepts every message.
    pub rxm: u8,
    #[bits(1, default = false)]
    _rsvd7: bool,
}

/// Receive mode: only messages matching an enabled filter are accepted.
pub(crate) const RXM_FILTERED: u8 = 0b00;
/// Receive mode: every message is accepted, filters and masks are ignored.
pub(crate) const RXM_ANY: u8 = 0b11;
