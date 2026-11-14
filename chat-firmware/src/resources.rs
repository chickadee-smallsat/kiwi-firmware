//! Peripheral resource assignment, trimmed to what the chat firmware
//! actually uses. The CAN pin assignments match the board's fixed wiring
//! (same as the main firmware's own `resources` module in `firmware/src`).

use assign_resources::assign_resources;
use embassy_rp::peripherals::{DMA_CH2, DMA_CH3, USB};
use embassy_rp::{Peri, bind_interrupts, peripherals};
use embassy_rp::{dma::InterruptHandler as DmaIrqHandler, usb::InterruptHandler as UsbIrqHandler};

assign_resources! {
    /// USB resources
    usbconfig: UsbConfDev {
        /// USB peripheral
        usb: USB,
    }

    /// CAN interface resources
    can: CanDev {
        /// Reset pin (active low)
        rst: PIN_16,
        /// CS pin (active low)
        cs: PIN_17,
        /// SPI clk
        clk: PIN_18,
        /// SPI data output pin
        tx: PIN_19,
        /// SPI data input pin
        rx: PIN_20,
        /// CAN refclk pin
        refclk: PIN_21,
        /// CAN interrupt pin
        irq: PIN_26,
        /// SPI peripheral for the CAN interface
        spi: SPI0,
        /// DMA channel for the CAN SPI interface's TX transfers
        dma_tx: DMA_CH2,
        /// DMA channel for the CAN SPI interface's RX transfers
        dma_rx: DMA_CH3,
    }
}

bind_interrupts!(
    /// Interrupt handlers for the peripherals used by this firmware.
    pub struct IrqHandlers {
    USBCTRL_IRQ => UsbIrqHandler<USB>;
    DMA_IRQ_0 => DmaIrqHandler<DMA_CH2>, DmaIrqHandler<DMA_CH3>;
});
