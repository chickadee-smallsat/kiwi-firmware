use assign_resources::assign_resources;
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, I2C0, PIO0, USB};
use embassy_rp::{Peri, bind_interrupts, peripherals};
use embassy_rp::{
    dma::InterruptHandler as DmaIrqHandler, i2c::InterruptHandler as I2cIrqHandler,
    pio::InterruptHandler as PioIrqHandler, usb::InterruptHandler as UsbIrqHandler,
};

assign_resources! {
    /// USB resources
    usbconfig: UsbConfDev {
        /// USB peripheral
        usb: USB,
    }

    /// Configuration update resources
    confdev: ConfigUpdateDev {
        /// FLASH peripheral for storing configuration data
        flash: FLASH,
        /// DMA channel for flash operations
        dma: DMA_CH0,
    }

    /// I2C resources for sensor communication
    i2cdev: I2cDev {
        /// I2C peripheral
        i2c: I2C0,
        /// I2C SCL pin
        scl: PIN_5,
        /// I2C SDA pin
        sda: PIN_4,
        /// Data ready IRQ pin for MMC5983MA magnetometer
        mmcirq: PIN_32,
        /// Data ready IRQ pin for BMI323 IMU
        imuirq: PIN_31,
        /// Data ready IRQ pin for BMP390 barometric sensor, not currently used
        baroirq: PIN_33,
    }

    /// Resources for the Wi-Fi module, including pins for gSPI communication and control signals
    wifi: WifiPins {
        /// gSPI CS
        cs: PIN_25,
        /// gSPI IO and IRQ
        io: PIN_24,
        /// gSPI CLK
        ck: PIN_29,
        /// Radio enable
        en: PIN_23,
        /// DMA channel for PIO data transfer
        dma: DMA_CH1,
        /// PIO instance for gSPI
        pio: PIO0,
    }
}

/// Type alias for the WiFi SPI interface using the assigned PIO and DMA resources.
pub type WifiSpi = cyw43_pio::PioSpi<'static, peripherals::PIO0, 0>;

bind_interrupts!(
    /// Interrupt handlers for the peripherals used in the firmware, mapped to their respective IRQs.
    pub struct IrqHandlers {
    I2C0_IRQ => I2cIrqHandler<I2C0>;
    PIO0_IRQ_0 => PioIrqHandler<PIO0>;
    USBCTRL_IRQ => UsbIrqHandler<USB>;
    DMA_IRQ_0 => DmaIrqHandler<DMA_CH0>, DmaIrqHandler<DMA_CH1>;
});
