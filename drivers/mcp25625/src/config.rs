//! Bit timing, acceptance filtering, and top-level device configuration.

use heapless::Vec;

use crate::frame::Id;
pub use crate::registers::OpMode;

/// CNF1/CNF2/CNF3 bit timing segments, in time quanta (TQ).
///
/// The nominal bit time is `(1 + prop_seg + phase_seg1 + phase_seg2) * TQ`,
/// where `TQ = 2 * (brp + 1) / Fosc` and `Fosc` is the frequency driving the
/// MCP25625's OSC1 pin (16 MHz when fed from this board's GPOUT reference
/// clock).
///
/// # Examples
///
/// At this board's 16 MHz reference clock, the three bit rates in common use
/// (250 kbps, 500 kbps, 1 Mbps) all divide the oscillator exactly:
///
/// ```
/// use mcp25625::BitTiming;
///
/// let osc_hz = 16_000_000;
/// for bitrate in [250_000, 500_000, 1_000_000] {
///     let bt = BitTiming::from_bit_rate(osc_hz, bitrate).unwrap();
///     assert_eq!(bt.bit_rate(osc_hz), bitrate);
/// }
/// ```
///
/// A rate the oscillator can't divide exactly still yields the closest
/// achievable configuration rather than `None`:
///
/// ```
/// use mcp25625::BitTiming;
///
/// let osc_hz = 16_000_000;
/// let bt = BitTiming::from_bit_rate(osc_hz, 123_456).unwrap();
/// let actual = bt.bit_rate(osc_hz);
/// assert!(actual.abs_diff(123_456) < 123_456 / 100); // within 1%
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct BitTiming {
    pub(crate) brp: u8,
    pub(crate) sjw: u8,
    pub(crate) prop_seg: u8,
    pub(crate) phase_seg1: u8,
    pub(crate) phase_seg2: u8,
    pub(crate) triple_sample: bool,
}

impl BitTiming {
    /// Builds a bit timing configuration from explicit segment lengths.
    ///
    /// - `sjw`: synchronization jump width, 1..=4 TQ, and must not exceed `phase_seg2`.
    /// - `brp`: baud rate prescaler, 0..=63.
    /// - `prop_seg`: propagation segment, 1..=8 TQ.
    /// - `phase_seg1`: phase segment 1, 1..=8 TQ.
    /// - `phase_seg2`: phase segment 2, 2..=8 TQ (must be >= the information
    ///   processing time of 2 TQ).
    ///
    /// Returns `None` if any parameter is out of range.
    pub const fn new(
        sjw: u8,
        brp: u8,
        prop_seg: u8,
        phase_seg1: u8,
        phase_seg2: u8,
        triple_sample: bool,
    ) -> Option<Self> {
        if sjw < 1 || sjw > 4 {
            return None;
        }
        if brp > 63 {
            return None;
        }
        if prop_seg < 1 || prop_seg > 8 {
            return None;
        }
        if phase_seg1 < 1 || phase_seg1 > 8 {
            return None;
        }
        if phase_seg2 < 2 || phase_seg2 > 8 {
            return None;
        }
        if sjw > phase_seg2 {
            return None;
        }
        Some(Self {
            brp,
            sjw,
            prop_seg,
            phase_seg1,
            phase_seg2,
            triple_sample,
        })
    }

    /// Derives a bit timing configuration for a target bit rate given the
    /// oscillator frequency (Hz) feeding OSC1, aiming for a sample point near
    /// 87.5% of the bit time.
    ///
    /// Searches time-quanta totals from 25 down to 8 (the valid MCP25625
    /// range) and keeps the configuration whose actual bit rate is closest
    /// to `bitrate_bps`. Returns `None` if `bitrate_bps` is zero or no
    /// configuration comes within range of the requested rate (e.g. the
    /// oscillator frequency can't be evenly divided down enough).
    pub fn from_bit_rate(osc_hz: u32, bitrate_bps: u32) -> Option<Self> {
        if bitrate_bps == 0 {
            return None;
        }
        let mut best: Option<Self> = None;
        let mut best_err = u32::MAX;
        let mut tq_total = 25u32;
        while tq_total >= 8 {
            let denom = 2u64 * bitrate_bps as u64 * tq_total as u64;
            let brp_plus_1 = (osc_hz as u64 + denom / 2) / denom;
            if (1..=64).contains(&brp_plus_1) {
                let actual = osc_hz / (2 * brp_plus_1 as u32 * tq_total);
                let err = actual.abs_diff(bitrate_bps);
                if err < best_err {
                    if let Some(bt) = Self::split_segments(tq_total, (brp_plus_1 - 1) as u8) {
                        best_err = err;
                        best = Some(bt);
                        if err == 0 {
                            break;
                        }
                    }
                }
            }
            tq_total -= 1;
        }
        best
    }

    /// Splits `tq_total - 1` time quanta between prop_seg/phase_seg1/phase_seg2
    /// targeting an ~87.5% sample point, for a given prescaler.
    fn split_segments(tq_total: u32, brp: u8) -> Option<Self> {
        let phase_total = tq_total - 1; // prop_seg + phase_seg1 + phase_seg2
                                        // phase_seg2 in [2, 8], and prop_seg + phase_seg1 in [2, 16]
        let lower = phase_total.saturating_sub(8).max(2);
        let upper = (phase_total - 2).min(16);
        if lower > upper {
            return None;
        }
        let target = ((phase_total * 875) / 1000).clamp(lower, upper);
        let phase_seg2 = phase_total - target;
        let mut prop_seg = (target / 2).max(1);
        if target - prop_seg > 8 {
            prop_seg = target - 8;
        }
        let phase_seg1 = target - prop_seg;
        let sjw = phase_seg2.min(4);
        Self::new(
            sjw as u8,
            brp,
            prop_seg as u8,
            phase_seg1 as u8,
            phase_seg2 as u8,
            false,
        )
    }

    /// Computes the actual bit rate this timing yields for the given oscillator frequency.
    pub const fn bit_rate(&self, osc_hz: u32) -> u32 {
        let tq_total = 1 + self.prop_seg as u32 + self.phase_seg1 as u32 + self.phase_seg2 as u32;
        osc_hz / (2 * (self.brp as u32 + 1) * tq_total)
    }
}

/// Receive acceptance filtering.
///
/// The MCP25625 has 2 masks (RXM0, RXM1) and 6 filters (RXF0/1 checked
/// against RXB0 under RXM0, RXF2..5 checked against RXB1 under RXM1). This
/// driver exposes exact-identifier matching per filter slot ([`RxFilter::Match`])
/// as well as an explicit partial mask shared by every slot
/// ([`RxFilter::Masked`]); use [`RxFilter::AcceptAll`] to disable filtering
/// entirely.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum RxFilter {
    /// Accept every received frame regardless of identifier.
    #[default]
    AcceptAll,
    /// Accept only frames whose identifier exactly matches one of the given
    /// identifiers (up to 6; standard and extended IDs may be mixed freely).
    /// Filter slots beyond the number of IDs given repeat the last ID.
    Match(Vec<Id, 6>),
    /// Accept only frames matching one of `ids` under a caller-supplied
    /// `mask` (`1` bit = must match, `0` bit = don't care), rather than the
    /// full-identifier exact match [`RxFilter::Match`] always applies. The
    /// same `mask` is written to both RXM0 and RXM1 — the MCP25625 only has
    /// the two, shared across all 6 filter slots, so a single mask is all
    /// this driver exposes. See [`crate::p2p::hardware_filter`] for a ready
    /// made mask/filter pair that accepts only frames addressed to one p2p
    /// peer or broadcast to every peer.
    Masked {
        /// Which identifier bits `ids` are compared against.
        mask: Id,
        /// Filter values compared against `mask` (up to 6). Filter slots
        /// beyond the number of IDs given repeat the last ID.
        ids: Vec<Id, 6>,
    },
}

/// Final operating mode to enter once initialization completes.
pub type OperatingMode = OpMode;

/// Top-level MCP25625 configuration.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Mcp25625Config {
    /// Arbitration bit timing.
    pub bit_timing: BitTiming,
    /// Mode to switch to once initialization completes.
    pub mode: OperatingMode,
    /// Receive acceptance filtering.
    pub filter: RxFilter,
    /// One-shot mode: do not automatically retry a transmission that lost
    /// arbitration or encountered an error.
    pub one_shot: bool,
    /// Roll a message over into RXB1 if it arrives while RXB0 is still full,
    /// instead of dropping it. Recommended when both RX buffers are enabled.
    pub rollover_on_overflow: bool,
    /// Enable the low-pass wake-up filter on bus activity during Sleep mode.
    pub wake_filter: bool,
}

impl Mcp25625Config {
    /// Starting point for a configuration: 500 kbps assuming a 16 MHz
    /// oscillator (matching this board's GPOUT-derived CAN reference clock),
    /// accept-all filtering, Normal mode, rollover enabled.
    pub fn new(bit_timing: BitTiming) -> Self {
        Self {
            bit_timing,
            mode: OpMode::Normal,
            filter: RxFilter::AcceptAll,
            one_shot: false,
            rollover_on_overflow: true,
            wake_filter: false,
        }
    }

    /// Sets the final operating mode.
    pub fn with_mode(mut self, mode: OperatingMode) -> Self {
        self.mode = mode;
        self
    }

    /// Sets the receive acceptance filter.
    pub fn with_filter(mut self, filter: RxFilter) -> Self {
        self.filter = filter;
        self
    }

    /// Enables or disables one-shot mode.
    pub fn with_one_shot(mut self, one_shot: bool) -> Self {
        self.one_shot = one_shot;
        self
    }
}
