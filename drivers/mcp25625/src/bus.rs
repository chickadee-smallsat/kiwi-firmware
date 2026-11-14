//! Low-level async SPI transactions implementing the MCP25625 instruction set.
//!
//! Each method here asserts CS for exactly one instruction (via
//! [`SpiDevice::transaction`], which brackets the whole slice of operations
//! in a single chip-select assertion), matching the datasheet's requirement
//! that CS stay low for the duration of an instruction.

use embedded_hal_async::spi::{Operation, SpiDevice};

use crate::registers::{
    INSTR_BIT_MODIFY, INSTR_LOAD_TX_BUFFER, INSTR_READ, INSTR_READ_RX_BUFFER, INSTR_READ_STATUS,
    INSTR_RESET, INSTR_RTS, INSTR_WRITE,
};

pub(crate) struct Bus<SPI> {
    pub(crate) spi: SPI,
}

impl<SPI, E> Bus<SPI>
where
    SPI: SpiDevice<Error = E>,
{
    /// Issues the RESET instruction ([`INSTR_RESET`]): resets internal
    /// registers to their power-on defaults and forces Configuration mode.
    /// See [`crate::driver::Mcp25625::init`] for why this is sent in
    /// addition to (not instead of) a hardware reset pulse on RST.
    pub(crate) async fn reset(&mut self) -> Result<(), E> {
        self.spi.write(&[INSTR_RESET]).await
    }

    /// Reads one byte from register `addr` via the READ instruction
    /// ([`INSTR_READ`]): writes `[INSTR_READ, addr]`, then clocks out the
    /// register's contents in the same CS assertion.
    pub(crate) async fn read_register(&mut self, addr: u8) -> Result<u8, E> {
        let mut buf = [0u8; 1];
        self.spi
            .transaction(&mut [
                Operation::Write(&[INSTR_READ, addr]),
                Operation::Read(&mut buf),
            ])
            .await?;
        Ok(buf[0])
    }

    /// Writes `value` to register `addr` via the WRITE instruction
    /// ([`INSTR_WRITE`]): `[INSTR_WRITE, addr, value]` in one transfer.
    pub(crate) async fn write_register(&mut self, addr: u8, value: u8) -> Result<(), E> {
        self.spi.write(&[INSTR_WRITE, addr, value]).await
    }

    /// Sets exactly the bits set in `mask` to the corresponding bits of `value`,
    /// leaving all other bits in the register untouched. Only usable on the
    /// subset of registers the datasheet marks as BIT MODIFY-capable
    /// (control/status registers, not TX/RX buffers or filters/masks).
    pub(crate) async fn modify_register(&mut self, addr: u8, mask: u8, value: u8) -> Result<(), E> {
        self.spi.write(&[INSTR_BIT_MODIFY, addr, mask, value]).await
    }

    /// Issues the READ STATUS instruction ([`INSTR_READ_STATUS`]), a
    /// single-byte shortcut for 8 commonly-polled flags that would otherwise
    /// require reading both CANINTF and all three TXBnCTRL registers. Bit
    /// layout of the returned status byte (bit 0 is the LSB):
    ///
    /// | bit | meaning                |
    /// |-----|------------------------|
    /// | 0   | CANINTF.RX0IF          |
    /// | 1   | CANINTF.RX1IF          |
    /// | 2   | TXB0CTRL.TXREQ         |
    /// | 3   | CANINTF.TX0IF          |
    /// | 4   | TXB1CTRL.TXREQ         |
    /// | 5   | CANINTF.TX1IF          |
    /// | 6   | TXB2CTRL.TXREQ         |
    /// | 7   | CANINTF.TX2IF          |
    ///
    /// [`crate::driver::Mcp25625::transmit`] uses the TXREQ bits (2, 4, 6) to
    /// find a free TX buffer without a separate register read per candidate.
    pub(crate) async fn read_status(&mut self) -> Result<u8, E> {
        let mut buf = [0u8; 1];
        self.spi
            .transaction(&mut [
                Operation::Write(&[INSTR_READ_STATUS]),
                Operation::Read(&mut buf),
            ])
            .await?;
        Ok(buf[0])
    }

    /// Loads TX buffer `idx` (0..=2) starting at its SIDH register with
    /// `header` (SIDH, SIDL, EID8, EID0, DLC) followed by `data`, via the
    /// LOAD TX BUFFER instruction ([`INSTR_LOAD_TX_BUFFER`]). This does not
    /// itself request transmission; follow with [`Self::request_to_send`].
    pub(crate) async fn load_tx_buffer(
        &mut self,
        idx: u8,
        header: [u8; 5],
        data: &[u8],
    ) -> Result<(), E> {
        let instr = INSTR_LOAD_TX_BUFFER | (idx << 1);
        self.spi
            .transaction(&mut [
                Operation::Write(&[instr]),
                Operation::Write(&header),
                Operation::Write(data),
            ])
            .await
    }

    /// Requests transmission of TX buffer `idx` (0..=2) via the RTS
    /// instruction ([`INSTR_RTS`]): sets that buffer's TXREQ bit, causing the
    /// controller to start arbitrating for the bus. Returns as soon as the
    /// request is sent, not once the frame actually goes out.
    pub(crate) async fn request_to_send(&mut self, idx: u8) -> Result<(), E> {
        self.spi.write(&[INSTR_RTS | (1 << idx)]).await
    }

    /// Reads RX buffer `idx` (0..=1) starting at its SIDH register into
    /// `header` (SIDH, SIDL, EID8, EID0, DLC) followed by `data` (8 bytes;
    /// only the first `dlc` are meaningful).
    pub(crate) async fn read_rx_buffer(
        &mut self,
        idx: u8,
        header: &mut [u8; 5],
        data: &mut [u8; 8],
    ) -> Result<(), E> {
        let instr = INSTR_READ_RX_BUFFER | (idx << 2);
        self.spi
            .transaction(&mut [
                Operation::Write(&[instr]),
                Operation::Read(header),
                Operation::Read(data),
            ])
            .await
    }
}
