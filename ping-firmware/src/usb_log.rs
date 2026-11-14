//! USB bring-up: just a defmt log sink on its own CDC-ACM interface. Unlike
//! `chat-firmware`'s `usb_chat` module, there's no second interactive
//! terminal interface here — this firmware only ever needs to be watched,
//! not typed into.

use embassy_executor::Spawner;
use embassy_rp::{peripherals::USB, usb::Driver as UsbDriver};
use embassy_usb::{Config, UsbDevice};
use static_cell::StaticCell;

use crate::resources::{IrqHandlers, UsbConfDev};

/// Device interface GUID advertised via the WCID registry property, so
/// host tooling can enumerate the reset interface with SetupAPI if it
/// wants to. Must match the `DeviceInterfaceGUIDs` value in
/// chickadee-probe-reset.inf.
const RESET_INTERFACE_GUID: &str = "{CDB3B5AD-293B-4663-AA36-1AAE46463776}";

type UsbDeviceDriver = UsbDevice<'static, UsbDriver<'static, USB>>;
type DefmtTask = embassy_defmt_usb::UsbDefmtTask<UsbDriver<'static, USB>>;

/// Builds the USB device (defmt log sink only) and spawns the tasks that
/// drive it.
pub fn build(spawner: Spawner, usbdev: UsbConfDev) {
    static USB_DEVICE: StaticCell<UsbDeviceDriver> = StaticCell::new();

    let driver = UsbDriver::new(usbdev.usb, IrqHandlers);

    let mut config = Config::new(0xc001, 0xbee7);
    config.manufacturer = Some("LoCSST/SKM");
    config.product = Some("Kiwi CAN Ping Firmware");
    config.serial_number = Some("2025-0003");
    config.max_power = 100;
    config.max_packet_size_0 = 64;

    // Build the embassy-usb Builder (descriptor buffers, MS OS 2.0/WCID
    // descriptor set, and the BOOTSEL reset handler) via `rp_usb_reset`.
    let mut usb_builder = rp_usb_reset::build_usb_builder!(driver, config, RESET_INTERFACE_GUID);

    // CDC-ACM: defmt log sink — interface and "defmt" string handler are
    // registered by `UsbDefmtLogger::build()`.
    let defmt_task = embassy_defmt_usb::UsbDefmtLogger::new().build(&mut usb_builder);

    let usb: UsbDevice<'_, UsbDriver<'_, USB>> = usb_builder.build();
    let usb_dev = USB_DEVICE.init(usb);
    spawner.spawn(defmt::unwrap!(usb_device_task(usb_dev)));
    spawner.spawn(defmt::unwrap!(defmt_drain_task(defmt_task)));
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
