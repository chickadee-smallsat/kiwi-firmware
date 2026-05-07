use core::{str::FromStr, sync::atomic::Ordering};

use defmt::*;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_rp::{
    flash::{Async, Flash},
    peripherals::{FLASH, USB},
    usb::Driver as UsbDriver,
};
use embassy_task_watchdog::embassy_rp::TaskWatchdog;
use embassy_time::{Duration, Timer};
use embassy_usb::{
    Builder, Config, UsbDevice,
    class::cdc_acm::{CdcAcmClass, State as CdcAcmState},
};
use heapless::String;
use kmdparse::{Parsable, parse};
use static_cell::StaticCell;

use device_blob::{BLOCK_SIZE, DeviceConfig, WifiConf, load_device_config};

use crate::{
    DEFAULT_DEVICE_NAME, DEFAULT_WIFI_HOST_AP, DataRateReplyReceiver, DataRateRequestSender,
    resources::{ConfigUpdateDev, IrqHandlers, UsbConfDev},
    wifi::WIFI_READY,
    wifi_fw_consts::{FLASH_SIZE, WIFI_CREDS_ADDR, WIFI_CREDS_OFST},
};

type CdcAcmDevice = CdcAcmClass<'static, UsbDriver<'static, USB>>;
type UsbDeviceDriver = UsbDevice<'static, UsbDriver<'static, USB>>;

/// Commands that can be sent over USB serial
#[derive(Debug, PartialEq, Eq, Parsable)]
pub enum Command {
    Help,
    Wifi(WifiCmd),
    Reset,
    Clear,
    Ident(Ident),
    Show,
    Store,
}

#[derive(Debug, PartialEq, Eq, Parsable)]
pub enum Ident {
    Set(String<12>),
    Get,
}

#[derive(Debug, PartialEq, Eq, Parsable)]
pub enum WifiCmd {
    Status,
    Ap {
        ssid: String<32>,
        password: Option<String<64>>,
    },
    Cl {
        ssid: String<32>,
        password: Option<String<64>>,
    },
}

pub fn usb_task(
    spawner: &Spawner,
    watchdog: TaskWatchdog,
    usbdev: UsbConfDev,
    flash: ConfigUpdateDev,
    request: DataRateRequestSender,
    reply: DataRateReplyReceiver,
) {
    static USB_DEVICE: StaticCell<UsbDeviceDriver> = StaticCell::new();
    static CDC_STATE: StaticCell<CdcAcmState> = StaticCell::new();
    static CDC_CLASS: StaticCell<CdcAcmDevice> = StaticCell::new();
    static CONF_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();

    let driver = UsbDriver::new(usbdev.usb, IrqHandlers);

    // Create embassy-usb Config
    let mut config = Config::new(0xc001, 0xbee5);
    config.manufacturer = Some("LoCSST/SKM");
    config.product = Some("Kiwi Mainboard Rev. B");
    config.serial_number = Some("2025-0001");
    config.max_power = 100;
    config.max_packet_size_0 = 64;

    // Create embassy-usb DeviceBuilder using the driver and config.
    // It needs some buffers for building the descriptors.
    let config_descriptor = CONF_DESC.init([0; 256]);
    let bos_descriptor = BOS_DESC.init([0; 256]);
    let control_buf = CONTROL_BUF.init([0; 64]);

    let reader_state = CDC_STATE.init(CdcAcmState::new());

    let mut usb_builder = Builder::new(
        driver,
        config,
        config_descriptor,
        bos_descriptor,
        &mut [], // no msos descriptors
        control_buf,
    );

    // Create classes on the builder.
    let comm = CDC_CLASS.init(CdcAcmClass::new(&mut usb_builder, reader_state, 64));
    trace!("Created CDC comm class");

    // Build the builder.
    let usb: UsbDevice<'_, UsbDriver<'_, USB>> = usb_builder.build();
    trace!("Built USB device");

    // Run the USB device.
    let usb_dev = USB_DEVICE.init(usb);
    spawner.spawn(usb_device_task(usb_dev)).unwrap();

    // Run the CDC comm task.
    spawner
        .spawn(usb_comm_task(comm, watchdog, flash, request, reply))
        .unwrap();
    trace!("Spawned USB tasks");
}

#[embassy_executor::task]
pub async fn usb_comm_task(
    dev: &'static mut CdcAcmDevice,
    watchdog: TaskWatchdog,
    flash: ConfigUpdateDev,
    request: DataRateRequestSender,
    reply: DataRateReplyReceiver,
) {
    let mut flash = {
        let mut flash = Flash::<_, Async, FLASH_SIZE>::new(flash.flash, flash.dma, IrqHandlers);
        // Test read at WiFi CLM address
        let mut buf = [0u8; 256];
        if let Err(e) = flash.blocking_read(WIFI_CREDS_OFST, &mut buf) {
            error!("Error reading flash at WiFi CLM address: {:?}", e);
            None
        } else {
            Some(flash)
        }
    };
    let mut data = [0u8; 256];
    let mut msg = String::<256>::new();
    let mut conf = load_device_config(WIFI_CREDS_ADDR).ok();
    loop {
        dev.wait_connection().await;
        trace!("USB Client Connected");
        while let Ok(n) = dev.read_packet(&mut data).await {
            if let Ok(s) = core::str::from_utf8(&data[..n]) {
                if dev.process_input(&mut msg, s).await.is_none() {
                    continue;
                }
                if let Ok(cmd) = parse::<_, Command>(msg.as_str(), ()) {
                    match cmd {
                        Command::Help => print_help_message(dev).await,
                        Command::Wifi(WifiCmd::Status) => {
                            match load_device_config(WIFI_CREDS_ADDR) {
                                Ok(ref conf) => {
                                    print_wifi_creds(dev, conf, false).await;
                                }
                                Err(e) => {
                                    dev.write_message(b"WiFi Credentials invalid: ").await;
                                    dev.write_message(e.as_bytes()).await;

                                    dev.write_message(b" WiFi process not running.\r\n").await;
                                }
                            };
                            if WIFI_READY.load(Ordering::Relaxed) {
                                let fut1 = async {
                                    request.send(()).await;
                                    debug!("Sent data rate request");
                                    let rate = reply.receive().await;
                                    debug!("Received data rate reply: {:?}", rate);
                                    write_packet_rate(dev, rate).await
                                };
                                let fut2 = Timer::after(Duration::from_secs(2));
                                match select(fut1, fut2).await {
                                    Either::First(_) => {}
                                    Either::Second(_) => {
                                        dev.write_message(b"Timed out waiting for data rate\r\n")
                                            .await;
                                    }
                                }
                            } else {
                                dev.write_message(b"WiFi not ready\r\n").await;
                            }
                        }
                        Command::Store => {
                            if let Some(conf) = conf.take() {
                                dev.write_message(b"Writing the following credentials:\r\n")
                                    .await;
                                print_wifi_creds(dev, &conf, true).await;
                                if let Err(e) = update_config(&mut flash, &conf) {
                                    e.write_message(dev).await;
                                } else {
                                    dev.write_message(
                                        b"Stored device credentials successfully.\r\n",
                                    )
                                    .await;
                                    reset(dev, watchdog).await;
                                }
                            } else {
                                dev.write_message(b"No new credentials to store.").await;
                                dev.write_message(b" Use ident <ident> or 'wifi ap <ssid> [pw]' or 'wifi cl <ssid> [pw]' to set new credentials.\r\n").await;
                            }
                        }
                        Command::Show => {
                            if let Some(ref conf) = conf {
                                print_wifi_creds(dev, conf, true).await;
                            } else {
                                dev.write_message(b"No device credentials set\r\n").await;
                            }
                        }
                        Command::Wifi(WifiCmd::Ap { ssid, password }) => {
                            match process_ssid_pw(ssid.as_str(), password.as_deref()) {
                                Ok((ssid_h, pw_h)) => {
                                    let cconf = WifiConf::Ap(ssid_h, pw_h);
                                    dev.write_message(b"WiFi AP credentials set\r\n").await;
                                    if let Some(ref mut conf) = conf {
                                        conf.wifi = cconf;
                                    } else {
                                        conf = Some(DeviceConfig {
                                            ident: String::from_str(DEFAULT_DEVICE_NAME).unwrap(),
                                            wifi: cconf,
                                        });
                                    }
                                    // SAFETY: We just set these credentials, so they should be valid to print
                                    print_wifi_creds(dev, conf.as_ref().unwrap(), true).await;
                                }
                                Err(e) => {
                                    dev.write_message(e.as_bytes()).await;
                                }
                            }
                        }
                        Command::Wifi(WifiCmd::Cl { ssid, password }) => {
                            match process_ssid_pw(ssid.as_str(), password.as_deref()) {
                                Ok((ssid_h, pw_h)) => {
                                    let cconf = WifiConf::Client(ssid_h, pw_h);
                                    dev.write_message(b"WiFi Client credentials set\r\n").await;
                                    if let Some(ref mut conf) = conf {
                                        conf.wifi = cconf;
                                    } else {
                                        conf = Some(DeviceConfig {
                                            ident: String::from_str(DEFAULT_DEVICE_NAME).unwrap(),
                                            wifi: cconf,
                                        });
                                    }
                                    // SAFETY: We just set these credentials, so they should be valid to print
                                    print_wifi_creds(dev, conf.as_ref().unwrap(), true).await;
                                }
                                Err(e) => {
                                    dev.write_message(e.as_bytes()).await;
                                }
                            }
                        }
                        Command::Ident(ident) => match ident {
                            Ident::Set(id) => {
                                if let Some(ref mut conf) = conf {
                                    conf.ident = id.clone();
                                } else {
                                    conf = Some(DeviceConfig {
                                        ident: id.clone(),
                                        wifi: WifiConf::Ap(
                                            String::from_str(DEFAULT_WIFI_HOST_AP).unwrap(),
                                            None,
                                        ), // default to AP mode with open network if ident is set before wifi creds
                                    });
                                }
                                // SAFETY: We just set this identifier, so it should be valid to print
                                let cconf = conf.as_ref().unwrap();
                                dev.write_message(b"Setting device identifier to: ").await;
                                dev.write_message(cconf.ident.as_bytes()).await;
                                dev.write_message(b"\r\n").await;
                            }
                            Ident::Get => {
                                if let Some(ref conf) = conf {
                                    dev.write_message(b"Device identifier: ").await;
                                    dev.write_message(conf.ident.as_bytes()).await;
                                    dev.write_message(b"\r\n").await;
                                } else {
                                    dev.write_message(b"No device identifier set\r\n").await;
                                }
                            }
                        },
                        Command::Reset => reset(dev, watchdog).await,
                        Command::Clear => dev.write_message(b"\x1b[2J\x1b[H").await, // ANSI escape codes to clear screen and move cursor to home
                    }
                } else {
                    dev.write_message(b"Unknown command").await;
                    dev.write_message(b": ").await;
                    dev.write_message(msg.as_bytes()).await;
                    dev.write_message(b"\r\nType 'help' for a list of commands.\r\n")
                        .await;
                }
                msg.clear();
                dev.write_message(b"\r\n> ").await; // prompt for next command
            } else {
                error!("Received invalid UTF-8");
            }
        }
        // let _ = echo(&mut class).await;
        trace!("USB Client Disconnected");
    }
}

#[embassy_executor::task]
pub async fn usb_device_task(dev: &'static mut UsbDeviceDriver) {
    dev.run().await;
}

fn process_ssid_pw(
    ssid: &str,
    pw: Option<&str>,
) -> Result<(String<32>, Option<String<64>>), &'static str> {
    if let Some(pw) = pw
        && pw.len() < 8
    {
        return Err("Error: Password too short (minimum 8 characters)");
    }
    let mut ssid_h = String::<32>::new();
    trace!("Processing SSID: {}, password: {}", ssid, pw);
    let _ = ssid_h.push_str(ssid); // Safety: checked length above
    let pw_h = pw.map(|p| {
        let mut s = String::<64>::new();
        let _ = s.push_str(p); // Safety: checked length above
        s
    });
    Ok((ssid_h, pw_h))
}

#[derive(Clone, PartialEq, Eq)]
enum UpdateConfigError {
    FlashNotAvailable,
    EraseError,
    WriteError,
    ReadError,
    DeserializeError,
    VerificationError(DeviceConfig),
}

impl UpdateConfigError {
    async fn write_message(&self, dev: &mut CdcAcmDevice) {
        match self {
            UpdateConfigError::FlashNotAvailable => {
                dev.write_message(
                    b"Error: Flash memory not available, cannot store WiFi credentials\r\n",
                )
                .await;
            }
            UpdateConfigError::EraseError => {
                dev.write_message(b"Error: Failed to erase flash memory\r\n")
                    .await;
            }
            UpdateConfigError::WriteError => {
                dev.write_message(b"Error: Failed to write to flash memory\r\n")
                    .await;
            }
            UpdateConfigError::ReadError => {
                dev.write_message(b"Error: Failed to read back from flash memory\r\n")
                    .await;
            }
            UpdateConfigError::DeserializeError => {
                dev.write_message(
                    b"Error: Failed to deserialize WiFi credentials from flash memory\r\n",
                )
                .await;
            }
            UpdateConfigError::VerificationError(wifi_conf) => {
                dev.write_message(b"Error: Verification failed, written WiFi credentials do not match intended values\r\n").await;
                dev.write_message(b"Written values:\r\n").await;
                print_wifi_creds(dev, wifi_conf, true).await;
            }
        }
    }
}

#[allow(clippy::result_large_err)]
fn update_config(
    flash: &mut Option<Flash<'_, FLASH, Async, FLASH_SIZE>>,
    conf: &DeviceConfig,
) -> Result<(), UpdateConfigError> {
    if let Some(flash) = flash {
        let mut buf = conf.to_bytes();
        flash
            .blocking_erase(WIFI_CREDS_OFST, WIFI_CREDS_OFST + BLOCK_SIZE as u32)
            .map_err(|e| {
                error!("Error erasing flash: {:?}", e);
                UpdateConfigError::EraseError
            })?;
        flash.blocking_write(WIFI_CREDS_OFST, &buf).map_err(|e| {
            error!("Error writing flash: {:?}", e);
            UpdateConfigError::WriteError
        })?;
        flash
            .blocking_read(WIFI_CREDS_OFST, &mut buf)
            .map_err(|e| {
                error!("Error reading flash: {:?}", e);
                UpdateConfigError::ReadError
            })?;
        let new_conf =
            DeviceConfig::from_bytes(&buf).map_err(|_| UpdateConfigError::DeserializeError)?;
        if conf != &new_conf {
            Err(UpdateConfigError::VerificationError(new_conf))
        } else {
            Ok(())
        }
    } else {
        Err(UpdateConfigError::FlashNotAvailable)
    }
}

async fn print_help_message(dev: &mut CdcAcmDevice) {
    dev.write_message(b"Kiwi Demo Firmware\r\n").await;
    dev.write_message(b"----------------------------\r\n").await;
    dev.write_message(b"Available commands:\r\n").await;
    dev.write_message(b"  help                - Show this help message\r\n")
        .await;
    dev.write_message(b"  ident get           - Show current device identifier\r\n")
        .await;
    dev.write_message(b"  ident set <ident>   - Set device identifier\r\n")
        .await;
    dev.write_message(b"  wifi status         - Show WiFi status\r\n")
        .await;
    dev.write_message(
        b"  wifi ap <ssid> [pw] - Set WiFi Access Point (WiFi Host) mode credentials\r\n",
    )
    .await;
    dev.write_message(b"  wifi cl <ssid> [pw] - Set WiFi Client mode credentials\r\n")
        .await;
    dev.write_message(b"  store               - Store device credentials to flash memory\r\n")
        .await;
    dev.write_message(b"  reset               - Reset the device\r\n")
        .await;
    dev.write_message(b"  clear               - Clear the terminal screen\r\n")
        .await;
    dev.write_message(b"\r\n").await;
}

async fn print_wifi_creds(dev: &mut CdcAcmDevice, conf: &DeviceConfig, show_pw: bool) {
    dev.write_message(b"Device ID: ").await;
    dev.write_message(conf.ident.as_bytes()).await;
    dev.write_message(b"\r\n").await;
    dev.write_message(b"WiFi Credentials:\r\n").await;
    let (ssid, pass) = match &conf.wifi {
        WifiConf::Ap(ssid, pass) => {
            dev.write_message(b"  Mode: Access Point\r\n").await;
            (ssid, pass)
        }
        WifiConf::Client(ssid, pass) => {
            dev.write_message(b"  Mode: Client\r\n").await;
            (ssid, pass)
        }
    };
    dev.write_message(b"  SSID: ").await;
    dev.write_message(ssid.as_bytes()).await;
    dev.write_message(b"\r\n").await;
    if let Some(pass) = pass {
        dev.write_message(b"  Password: ").await;
        if show_pw {
            dev.write_message(pass.as_bytes()).await;
        } else {
            dev.write_message(b"****").await;
        }
        dev.write_message(b"\r\n").await;
    } else {
        dev.write_message(b"  Open network\r\n").await;
    }
}

async fn reset(dev: &mut CdcAcmDevice, watchdog: TaskWatchdog) {
    dev.write_message(b"Resetting device...\r\n").await;
    Timer::after(Duration::from_secs(1)).await;
    watchdog
        .trigger_reset(Some(String::from_str("UsbCommand").unwrap()))
        .await;
}

async fn write_packet_rate(dev: &mut CdcAcmDevice, rate: Option<(f32, &'static str, f32)>) {
    if let Some((rate, unit, pkt)) = rate {
        dev.write_message(b"Data rate: ").await;
        let mut buffer = dtoa::Buffer::new();
        let rate = buffer.format(rate);
        dev.write_message(rate.as_bytes()).await;
        dev.write_message(b" ").await;
        dev.write_message(unit.as_bytes()).await;
        dev.write_message(b", Packet rate: ").await;
        let mut buffer = dtoa::Buffer::new();
        let pkt = buffer.format(pkt);
        dev.write_message(pkt.as_bytes()).await;
        dev.write_message(b" pkt/s\r\n").await;
    } else {
        dev.write_message(b"No data sent/received yet\r\n").await;
    }
}

trait AcmDeviceFunctions {
    async fn write_message(&mut self, bytes: &[u8]);
    async fn process_input<const N: usize>(
        &mut self,
        msg: &mut String<N>,
        input: &str,
    ) -> Option<()>;
}

impl AcmDeviceFunctions for CdcAcmDevice {
    async fn write_message(&mut self, s: &[u8]) {
        let iter = s.chunks(32);
        for chunk in iter {
            let _ = self.write_packet(chunk).await;
        }
    }

    async fn process_input<const N: usize>(
        &mut self,
        msg: &mut String<N>,
        input: &str,
    ) -> Option<()> {
        let mut skip = 0;
        for c in input.chars() {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            if c.is_control() {
                if c == '\x08' || c == '\x7f' {
                    // backspace or delete
                    if msg.pop().is_some() {
                        // Move cursor back, print space, move cursor back again
                        self.write_message(b"\x08 \x08").await;
                    }
                } else if c == '\n' || c == '\r' {
                    // end of command
                    if !msg.is_empty() {
                        info!("Received command: {}", msg);
                        self.write_message(b"\r\n").await; // extra newline after echo for readability
                        return Some(());
                    } else {
                        self.write_message(b"\r\n> ").await; // prompt for next command
                    }
                } else if c == '\x1b' {
                    // Ignore other control characters
                    skip = 2; // rudimentary way to skip ANSI escape sequences
                    continue;
                }
            } else if msg.push(c).is_err() {
                self.write_message(("Error: message too large, discarding.\r\n").as_bytes())
                    .await;
                msg.clear();
                self.write_message(b"> ").await; // extra newline after echo for readability
            } else {
                let mut buf = [0u8; 4];
                self.write_message(c.encode_utf8(&mut buf).as_bytes()).await; // echo back to tty0
            }
        }
        None
    }
}
