//! USB bring-up: a defmt log sink on its own CDC-ACM interface (mirroring
//! the main firmware's `usb` module), plus a second CDC-ACM interface for
//! the interactive chat terminal, split into a sender/receiver pair so
//! `chat_task` can read input and print incoming chat messages without
//! either blocking the other.

use embassy_executor::Spawner;
use embassy_rp::{peripherals::USB, usb::Driver as UsbDriver};
use embassy_usb::{
    Config, UsbDevice,
    class::cdc_acm::{CdcAcmClass, Receiver, Sender, State as CdcAcmState},
};
use static_cell::StaticCell;

use crate::resources::{IrqHandlers, UsbConfDev};

/// Device interface GUID advertised via the WCID registry property, so
/// host tooling can enumerate the reset interface with SetupAPI if it
/// wants to. Must match the `DeviceInterfaceGUIDs` value in
/// chickadee-probe-reset.inf.
const RESET_INTERFACE_GUID: &str = "{CDB3B5AD-293B-4663-AA36-1AAE46463776}";

type UsbDeviceDriver = UsbDevice<'static, UsbDriver<'static, USB>>;
type DefmtTask = embassy_defmt_usb::UsbDefmtTask<UsbDriver<'static, USB>>;

/// Write half of the chat CDC-ACM interface.
pub type ChatSender = Sender<'static, UsbDriver<'static, USB>>;
/// Read half of the chat CDC-ACM interface.
pub type ChatReceiver = Receiver<'static, UsbDriver<'static, USB>>;

/// Builds the USB device (defmt log sink + chat CDC-ACM interface), spawns
/// the tasks that drive it, and returns the chat interface split into a
/// sender/receiver pair for `chat_task`.
pub fn build(spawner: Spawner, usbdev: UsbConfDev) -> (ChatSender, ChatReceiver) {
    static USB_DEVICE: StaticCell<UsbDeviceDriver> = StaticCell::new();
    static CDC_STATE: StaticCell<CdcAcmState> = StaticCell::new();

    let driver = UsbDriver::new(usbdev.usb, IrqHandlers);

    let mut config = Config::new(0xc001, 0xbee6);
    config.manufacturer = Some("LoCSST/SKM");
    config.product = Some("Kiwi Chat Firmware");
    config.serial_number = Some("2025-0002");
    config.max_power = 100;
    config.max_packet_size_0 = 64;

    // Build the embassy-usb Builder (descriptor buffers, MS OS 2.0/WCID
    // descriptor set, and the BOOTSEL reset handler) via `rp_usb_reset`.
    let mut usb_builder = rp_usb_reset::build_usb_builder!(driver, config, RESET_INTERFACE_GUID);

    let reader_state = CDC_STATE.init(CdcAcmState::new());

    // CDC-ACM #1: the chat terminal. Owned locally (not a `StaticCell`) so it
    // can be consumed by `split()` below — its endpoints are already tied to
    // `'static` since every descriptor buffer above is.
    let comm = CdcAcmClass::new(&mut usb_builder, reader_state, 64);

    // CDC-ACM #2: defmt log sink — interface and "defmt" string handler are
    // registered by `UsbDefmtLogger::build()`.
    let defmt_task = embassy_defmt_usb::UsbDefmtLogger::new().build(&mut usb_builder);

    let usb: UsbDevice<'_, UsbDriver<'_, USB>> = usb_builder.build();
    let usb_dev = USB_DEVICE.init(usb);
    spawner.spawn(defmt::unwrap!(usb_device_task(usb_dev)));
    spawner.spawn(defmt_drain_task(defmt_task).unwrap());

    comm.split()
}

/// Embassy task that drains defmt bytes over the CDC-ACM bulk-IN endpoint.
#[embassy_executor::task]
async fn defmt_drain_task(task: DefmtTask) {
    task.run().await;
}

#[embassy_executor::task]
async fn usb_device_task(dev: &'static mut UsbDeviceDriver) {
    dev.run().await;
}
