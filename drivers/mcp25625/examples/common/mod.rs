//! Mock MCP25625 "hardware" shared by every example in this directory.
//!
//! `mcp25625` is a `no_std` driver written directly against the chip's real
//! SPI instruction set (see `src/bus.rs`/`src/registers.rs`) — it has no
//! concept of a mock or a simulator. To run its examples on a host with no
//! actual CAN controller attached, this module plays the part of the chip
//! itself: it implements the same `embedded_hal`/`embedded_hal_async`
//! traits (`SpiDevice`, `OutputPin`, `InputPin`, `Wait`) that a real SPI
//! peripheral and pair of GPIOs would, backed by a tiny software model of
//! just enough MCP25625 register behavior for [`Mcp25625::init`],
//! [`Mcp25625::transmit`]-equivalent, and interrupt-driven receive to work
//! exactly as they would against silicon.
//!
//! [`virtual_pair`] wires up two such mock chips back to back, standing in
//! for two boards sharing a physical CAN bus. Two simplifications versus
//! real hardware, both deliberate:
//!
//! - Acceptance filtering (`RXM`/`RXF`) is stored but never enforced — every
//!   frame that reaches a mock chip is accepted, as if `RxFilter::AcceptAll`
//!   were configured regardless of what was actually written. This is safe
//!   to skip here because `P2pReceiver`/`IsoTpChannel` always re-check the
//!   destination/ID in software regardless of what hardware filtering would
//!   have discarded; a mock that accepts everything is still *correct*,
//!   just less efficient than real filtered hardware would be.
//! - Bit timing (`CNF1`-`CNF3`) is stored but never used to model actual bit
//!   rate/arbitration — there's no analog bus here, just an in-process queue
//!   standing in for one.
//!
//! Only what each example actually calls through `mcp25625::driver::Mcp25625`
//! is implemented; this is not a general-purpose MCP2515/MCP25625 simulator.
#![allow(dead_code)]

use std::{cell::RefCell, convert::Infallible, rc::Rc};

use embassy_sync::{
    blocking_mutex::raw::NoopRawMutex,
    channel::{Channel, Receiver, Sender},
    signal::Signal,
};
use embedded_hal::digital::{self, InputPin, OutputPin};
use embedded_hal_async::{
    digital::Wait,
    spi::{ErrorType as SpiErrorType, Operation, SpiDevice},
};

/// Depth of the virtual bus queue between two mock chips. Generous relative
/// to anything these examples send in a burst, so a full queue would
/// indicate a real bug rather than expected backpressure.
const WIRE_DEPTH: usize = 32;

// ---------------------------------------------------------------------
// SPI instructions and registers this mock understands.
//
// These mirror the private constants in `mcp25625::registers` (not visible
// from here, since examples only see the crate's public API) — duplicated
// because a mock chip has to speak the same wire protocol the real one
// does. See that module for the authoritative bit-level documentation.
// ---------------------------------------------------------------------
const INSTR_RESET: u8 = 0xC0;
const INSTR_READ: u8 = 0x03;
const INSTR_WRITE: u8 = 0x02;
const INSTR_BIT_MODIFY: u8 = 0x05;
const INSTR_READ_STATUS: u8 = 0xA0;

const REG_CANSTAT: u8 = 0x0E;
const REG_CANCTRL: u8 = 0x0F;
const REG_CANINTE: u8 = 0x2B;
const REG_CANINTF: u8 = 0x2C;

/// One CAN frame's worth of raw SPI bytes, exactly as loaded into/read out of
/// a TX/RX buffer (`SIDH, SIDL, EID8, EID0, DLC` followed by up to 8 data
/// bytes) — what actually travels across [`virtual_pair`]'s wire, so this
/// mock never needs to know how to interpret an [`Id`](mcp25625::Id) itself.
#[derive(Clone, Copy, Default)]
pub struct RawFrame {
    header: [u8; 5],
    data: [u8; 8],
    len: u8,
}

/// Software model of just the MCP25625 register/buffer state this crate's
/// driver actually touches. See the module docs for what's deliberately not
/// modeled (filtering, bit timing).
struct ChipState {
    regs: [u8; 0x80],
    tx_slots: [Option<RawFrame>; 3],
    rx_bufs: [Option<RawFrame>; 2],
}

impl ChipState {
    fn new() -> Self {
        let mut regs = [0u8; 0x80];
        // Real hardware powers up (and resets into) Configuration mode;
        // `Mcp25625::init` checks for exactly this before doing anything else.
        regs[REG_CANSTAT as usize] = 0x80;
        Self {
            regs,
            tx_slots: [None; 3],
            rx_bufs: [None; 2],
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn read(&self, addr: u8) -> u8 {
        self.regs[addr as usize]
    }

    fn write(&mut self, addr: u8, value: u8) {
        self.regs[addr as usize] = value;
        if addr == REG_CANCTRL {
            // CANCTRL.REQOP and CANSTAT.OPMOD share the same top-3-bit
            // position in both registers; real hardware confirms a mode
            // change almost immediately, so this mock just reflects it
            // straight through rather than modeling the transition delay.
            self.regs[REG_CANSTAT as usize] =
                (self.regs[REG_CANSTAT as usize] & 0x1F) | (value & 0xE0);
        }
        if addr == REG_CANINTF {
            self.sync_rx_bufs();
        }
    }

    fn bit_modify(&mut self, addr: u8, mask: u8, value: u8) {
        let cur = self.regs[addr as usize];
        self.regs[addr as usize] = (cur & !mask) | (value & mask);
        if addr == REG_CANINTF {
            self.sync_rx_bufs();
        }
    }

    /// On real hardware, clearing CANINTF.RXnIF is what frees RXBn for the
    /// next message (the driver always reads a buffer before clearing its
    /// flag, never the other way around). Mirror that here: without it,
    /// `rx_bufs` would fill up permanently after the first message ever
    /// received on each buffer, silently dropping everything after.
    fn sync_rx_bufs(&mut self) {
        let intf = self.regs[REG_CANINTF as usize];
        if intf & 0b01 == 0 {
            self.rx_bufs[0] = None;
        }
        if intf & 0b10 == 0 {
            self.rx_bufs[1] = None;
        }
    }

    /// Bit 0 = CANINTF.RX0IF, bit 1 = CANINTF.RX1IF; the TXREQ/TXnIF bits
    /// `Bus::read_status` also exposes are always 0 here, since this mock
    /// completes every transmission instantly (see `take_tx`) and the
    /// driver never enables TX-complete interrupts.
    fn status_byte(&self) -> u8 {
        self.regs[REG_CANINTF as usize] & 0b11
    }

    /// The INT pin is level low while any enabled interrupt source is
    /// latched — real hardware behavior [`Mcp25625::receive`] relies on.
    fn irq_pending(&self) -> bool {
        self.regs[REG_CANINTF as usize] & self.regs[REG_CANINTE as usize] != 0
    }

    fn load_tx(&mut self, idx: usize, header: [u8; 5], data: &[u8]) {
        let mut buf = [0u8; 8];
        buf[..data.len()].copy_from_slice(data);
        self.tx_slots[idx] = Some(RawFrame {
            header,
            data: buf,
            len: data.len() as u8,
        });
    }

    fn take_tx(&mut self, idx: usize) -> Option<RawFrame> {
        self.tx_slots[idx].take()
    }

    /// Delivers a frame that arrived over the virtual bus into RXB0, falling
    /// back to RXB1 if RXB0 is still unread (mirroring `BUKT` rollover), or
    /// dropping it if both are full (a real overflow would also latch EFLG,
    /// which this mock doesn't bother modeling — nothing here reads it).
    fn deliver(&mut self, frame: RawFrame) {
        let (slot, bit) = if self.rx_bufs[0].is_none() {
            (0, 0b01)
        } else if self.rx_bufs[1].is_none() {
            (1, 0b10)
        } else {
            return;
        };
        self.rx_bufs[slot] = Some(frame);
        self.regs[REG_CANINTF as usize] |= bit;
    }

    fn read_rx(&self, idx: usize) -> RawFrame {
        self.rx_bufs[idx].expect("driver only reads RXBn after its CANINTF flag is set")
    }
}

/// A mock chip's state plus the async notification its INT pin waits on.
/// Shared (via `Rc`, since every example here is single-threaded — one
/// `embassy_executor::Executor` running all of one process's node and pump
/// tasks cooperatively) between the [`MockSpi`] and [`MockIrq`] a node owns
/// and the [`bus_pump`] task that feeds it from the virtual bus.
pub struct Chip {
    state: RefCell<ChipState>,
    irq: Signal<NoopRawMutex, ()>,
}

/// Stands in for the SPI peripheral (wrapping the CS pin, as a real
/// `embedded_hal_async::spi::SpiDevice` would) — see the module docs for
/// which instructions are understood and what's deliberately not modeled.
pub struct MockSpi {
    chip: Rc<Chip>,
    tx: Sender<'static, NoopRawMutex, RawFrame, WIRE_DEPTH>,
}

impl SpiErrorType for MockSpi {
    type Error = Infallible;
}

impl SpiDevice for MockSpi {
    async fn transaction(&mut self, ops: &mut [Operation<'_, u8>]) -> Result<(), Infallible> {
        let (opcode, a1, a2, a3) = match ops.first() {
            Some(Operation::Write(cmd)) => (
                cmd[0],
                cmd.get(1).copied(),
                cmd.get(2).copied(),
                cmd.get(3).copied(),
            ),
            _ => return Ok(()),
        };
        match opcode {
            INSTR_RESET => self.chip.state.borrow_mut().reset(),
            INSTR_READ => {
                let value = self
                    .chip
                    .state
                    .borrow()
                    .read(a1.expect("READ carries an address"));
                if let Some(Operation::Read(buf)) = ops.get_mut(1) {
                    buf[0] = value;
                }
            }
            INSTR_WRITE => self.chip.state.borrow_mut().write(
                a1.expect("WRITE carries an address"),
                a2.expect("WRITE carries a value"),
            ),
            INSTR_BIT_MODIFY => self.chip.state.borrow_mut().bit_modify(
                a1.expect("BIT MODIFY carries an address"),
                a2.expect("BIT MODIFY carries a mask"),
                a3.expect("BIT MODIFY carries a value"),
            ),
            INSTR_READ_STATUS => {
                let value = self.chip.state.borrow().status_byte();
                if let Some(Operation::Read(buf)) = ops.get_mut(1) {
                    buf[0] = value;
                }
            }
            // LOAD TX BUFFER (idx 0..=2, always starting at SIDH).
            0x40 | 0x42 | 0x44 => {
                let idx = ((opcode - 0x40) / 2) as usize;
                let (head, tail) = ops.split_at_mut(2);
                if let (Operation::Write(header), Operation::Write(data)) = (&head[1], &tail[0]) {
                    let mut hdr = [0u8; 5];
                    hdr.copy_from_slice(header);
                    self.chip.state.borrow_mut().load_tx(idx, hdr, data);
                }
            }
            // RTS (idx 0..=2, one bit per buffer).
            0x81 | 0x82 | 0x84 => {
                let idx = match opcode {
                    0x81 => 0,
                    0x82 => 1,
                    _ => 2,
                };
                if let Some(frame) = self.chip.state.borrow_mut().take_tx(idx) {
                    // A full wire would mean this demo is sending far more
                    // than expected; drop rather than block, same as a real
                    // transmit that never wins arbitration in time.
                    let _ = self.tx.try_send(frame);
                }
            }
            // READ RX BUFFER (idx 0..=1, always starting at SIDH).
            0x90 | 0x94 => {
                let idx = usize::from(opcode == 0x94);
                let frame = self.chip.state.borrow().read_rx(idx);
                let (head, tail) = ops.split_at_mut(2);
                if let (Operation::Read(header), Operation::Read(data)) =
                    (&mut head[1], &mut tail[0])
                {
                    header.copy_from_slice(&frame.header);
                    data[..frame.len as usize].copy_from_slice(&frame.data[..frame.len as usize]);
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// Stands in for the RST GPIO. A real hardware reset pulse is redundant with
/// the SPI-level RESET instruction `Mcp25625::init` also sends (see that
/// method's doc comment) — since this mock's only reset behavior lives in
/// [`ChipState::reset`], the pin itself has nothing to do.
pub struct MockRst;

impl digital::ErrorType for MockRst {
    type Error = Infallible;
}

impl OutputPin for MockRst {
    fn set_low(&mut self) -> Result<(), Infallible> {
        Ok(())
    }
    fn set_high(&mut self) -> Result<(), Infallible> {
        Ok(())
    }
}

/// Stands in for the INT GPIO: level-low for as long as any enabled
/// interrupt source is latched, exactly like the real pin.
pub struct MockIrq {
    chip: Rc<Chip>,
}

impl digital::ErrorType for MockIrq {
    type Error = Infallible;
}

impl InputPin for MockIrq {
    fn is_high(&mut self) -> Result<bool, Infallible> {
        Ok(!self.chip.state.borrow().irq_pending())
    }
    fn is_low(&mut self) -> Result<bool, Infallible> {
        Ok(self.chip.state.borrow().irq_pending())
    }
}

impl MockIrq {
    async fn wait_level(&self, low: bool) {
        loop {
            if self.chip.state.borrow().irq_pending() == low {
                return;
            }
            self.chip.irq.wait().await;
        }
    }
}

impl Wait for MockIrq {
    async fn wait_for_high(&mut self) -> Result<(), Infallible> {
        self.wait_level(false).await;
        Ok(())
    }
    async fn wait_for_low(&mut self) -> Result<(), Infallible> {
        self.wait_level(true).await;
        Ok(())
    }
    async fn wait_for_rising_edge(&mut self) -> Result<(), Infallible> {
        self.wait_level(true).await;
        self.wait_level(false).await;
        Ok(())
    }
    async fn wait_for_falling_edge(&mut self) -> Result<(), Infallible> {
        self.wait_level(false).await;
        self.wait_level(true).await;
        Ok(())
    }
    async fn wait_for_any_edge(&mut self) -> Result<(), Infallible> {
        let start = self.chip.state.borrow().irq_pending();
        loop {
            if self.chip.state.borrow().irq_pending() != start {
                return Ok(());
            }
            self.chip.irq.wait().await;
        }
    }
}

/// A real `embedded_hal_async::delay::DelayNs` backed by `embassy-time`'s
/// `std` time driver — the same [`embassy_time::Delay`] firmware uses, just
/// running against wall-clock time instead of a hardware timer.
pub type MockDelay = embassy_time::Delay;

/// One node's worth of mock peripherals — everything [`mcp25625::Mcp25625::new`]
/// needs, plus the pieces [`bus_pump`] needs to keep feeding it.
pub struct NodeHardware {
    /// Pass to `Mcp25625::new` as the SPI device.
    pub spi: MockSpi,
    /// Pass to `Mcp25625::new` as the RST pin.
    pub rst: MockRst,
    /// Pass to `Mcp25625::new` as the IRQ pin.
    pub irq: MockIrq,
    /// Pass to `Mcp25625::new` as the delay provider.
    pub delay: MockDelay,
    /// Spawn `bus_pump(chip, inbox)` alongside this node's other tasks
    /// before calling `Mcp25625::init` — nothing arrives over the virtual
    /// bus until that task is running.
    pub chip: Rc<Chip>,
    /// See [`Self::chip`].
    pub inbox: Receiver<'static, NoopRawMutex, RawFrame, WIRE_DEPTH>,
}

/// Feeds frames arriving over the virtual bus into a mock chip's RX buffers
/// and wakes its INT pin, forever. Every node in these examples must spawn
/// exactly one of these (see [`NodeHardware::chip`]) before the corresponding
/// driver's `init()`/`receive()` will see anything the other side sends.
#[embassy_executor::task(pool_size = 2)]
pub async fn bus_pump(
    chip: Rc<Chip>,
    inbox: Receiver<'static, NoopRawMutex, RawFrame, WIRE_DEPTH>,
) -> ! {
    loop {
        let frame = inbox.receive().await;
        chip.state.borrow_mut().deliver(frame);
        chip.irq.signal(());
    }
}

/// Wires up two mock MCP25625 chips back to back, standing in for two
/// boards sharing a physical CAN bus. Every frame one side's driver
/// transmits shows up as received on the other's, and vice versa.
///
/// Leaks the two virtual-bus channels (`Box::leak`) to get the `'static`
/// lifetime `embassy_sync::channel::Channel`'s `Sender`/`Receiver` need —
/// harmless in a short-lived example process, but not a pattern to copy into
/// firmware (use `static_cell::StaticCell` there instead, as `firmware/`
/// itself does).
pub fn virtual_pair() -> (NodeHardware, NodeHardware) {
    let a_to_b: &'static Channel<NoopRawMutex, RawFrame, WIRE_DEPTH> =
        Box::leak(Box::new(Channel::new()));
    let b_to_a: &'static Channel<NoopRawMutex, RawFrame, WIRE_DEPTH> =
        Box::leak(Box::new(Channel::new()));

    let chip_a = Rc::new(Chip {
        state: RefCell::new(ChipState::new()),
        irq: Signal::new(),
    });
    let chip_b = Rc::new(Chip {
        state: RefCell::new(ChipState::new()),
        irq: Signal::new(),
    });

    let node_a = NodeHardware {
        spi: MockSpi {
            chip: chip_a.clone(),
            tx: b_to_a.sender(),
        },
        rst: MockRst,
        irq: MockIrq {
            chip: chip_a.clone(),
        },
        delay: embassy_time::Delay,
        chip: chip_a,
        inbox: a_to_b.receiver(),
    };
    let node_b = NodeHardware {
        spi: MockSpi {
            chip: chip_b.clone(),
            tx: a_to_b.sender(),
        },
        rst: MockRst,
        irq: MockIrq {
            chip: chip_b.clone(),
        },
        delay: embassy_time::Delay,
        chip: chip_b,
        inbox: b_to_a.receiver(),
    };
    (node_a, node_b)
}
