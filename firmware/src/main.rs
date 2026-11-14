#![no_std]
#![no_main]
#![allow(unused_imports)]
use defmt::{debug, info, trace, warn};
use device_blob::load_device_config;
use embassy_sync::channel::Channel;
use heapless::String;
use kiwi_firmware_base::{
    DEFAULT_DEVICE_NAME, DEFAULT_WIFI_HOST_AP, DEVICE_ID, DataRateReplyChannel,
    DataRateRequestChannel, MeasurementChannel,
};
use portable_atomic::AtomicU64;
// Re-export lib types so submodules can use `crate::` to access them.
use embassy_executor::{Executor, Spawner};
use embassy_rp::multicore::{Stack, spawn_core1};
pub use embassy_task_watchdog::embassy_rp::TaskWatchdog;
use embassy_task_watchdog::{
    ResetReason, WatchdogConfig, create_watchdog, embassy_rp::WatchdogRunner,
};
use embassy_time::{Duration, Instant, Timer};
pub use kiwi_firmware_base::{
    DataRateReplyReceiver, DataRateReplySender, DataRateRequestReceiver, DataRateRequestSender,
    MEASUREMENT_CHANNEL_SIZE, MeasurementReceiver, MeasurementSender,
};
use static_cell::StaticCell;

use core::str::FromStr as _;
use panic_probe as _;

mod ble;
mod bootsel;
mod fw_consts;
mod localstor;
mod resources;
mod sensor_tasks;
mod usb;
mod wifi;

use crate::fw_consts::WIFI_CREDS_ADDR;
use crate::resources::ConfigUpdateDev;
use crate::{
    resources::{AssignedResources, BootselDev, CanDev, I2cDev, MmcPins, UsbConfDev, WifiPins},
    sensor_tasks::spawn_sensor_task,
};

static BOOT_TIME: AtomicU64 = AtomicU64::new(0);

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    #[cfg(feature = "cpuinfo")]
    {
        embassy_cpu_usage::init();
        spawner.spawn(cpu_report().unwrap());
    }
    let start_time = Instant::now();
    BOOT_TIME.store(start_time.as_ticks(), core::sync::atomic::Ordering::Relaxed);
    let p = embassy_rp::init(Default::default());
    let resources = split_resources!(p);

    // Create the watchdog instance
    let config = WatchdogConfig::new(Duration::from_secs(5), Duration::from_secs(2));
    let (watchdog, watchdogrunner) = create_watchdog!(p.WATCHDOG, config);

    /// Set up a channel for sending measurements from sensor tasks to the storage task
    static DATA_CHANNEL: StaticCell<MeasurementChannel> = StaticCell::new();
    let channel = DATA_CHANNEL.init(Channel::new());

    /// Set up a channel for sending data rate requests from USB task to Wi-Fi task
    static DR_REQUEST_CHANNEL: StaticCell<DataRateRequestChannel> = StaticCell::new();
    let dr_request_channel = DR_REQUEST_CHANNEL.init(Channel::new());
    let rq_recv = dr_request_channel.receiver();
    let rq_send = dr_request_channel.sender();

    /// Set up a channel for sending data rate replies from Wi-Fi task to USB task
    static DR_REPLY_CHANNEL: StaticCell<DataRateReplyChannel> = StaticCell::new();
    let dr_reply_channel = DR_REPLY_CHANNEL.init(Channel::new());
    let rp_recv = dr_reply_channel.receiver();
    let rp_send = dr_reply_channel.sender();

    /// Set up a channel for the MMC task to get measurements from the sensor tasks
    static MMC_CHANNEL: StaticCell<MeasurementChannel> = StaticCell::new();
    let mmc_channel = MMC_CHANNEL.init(Channel::new());
    let mmc_receiver = mmc_channel.receiver();
    let mmc_sender = mmc_channel.sender();

    // Set up flash memory
    // add some delay to give an attached debug probe time to parse the
    // defmt RTT header. Reading that header might touch flash memory, which
    // interferes with flash write operations.
    // https://github.com/knurling-rs/defmt/pull/683
    Timer::after_millis(10).await;

    // Load Device ID from flash and store it in the global variable
    let conf = load_device_config(WIFI_CREDS_ADDR);
    match conf {
        Ok(ref conf) => debug!("Device credentials loaded from flash: {:?}", conf),
        Err(e) => warn!(
            "Failed to load device credentials from flash: {}! Were the device credentials flashed?",
            e
        ),
    }
    {
        let mut dev_id = DEVICE_ID.lock().await;
        dev_id.clear();
        dev_id
            .push_str(
                &conf
                    .map(|c| c.ident)
                    .unwrap_or(String::from_str(DEFAULT_DEVICE_NAME).unwrap()),
            )
            .unwrap(); // SAFETY: The device ID is guaranteed to be less than 32 characters
    }

    let receiver = channel.receiver();
    let sender = channel.sender();

    // Spawn the USB task
    usb::usb_task(
        &spawner,
        watchdog,
        resources.usbconfig,
        resources.confdev,
        rq_send,
        rp_recv,
    );

    // Add a delay to ensure USB task has time to initialize before proceeding
    Timer::after(Duration::from_millis(1000)).await;

    info!("Kiwi Demo FW v{} starting up...", env!("CARGO_PKG_VERSION"));
    match watchdog.reset_reason().await {
        ResetReason::Forced(reason) => info!("Forced reset, reason: {}", reason),
        ResetReason::TimedOut(task) => info!("Task timed out: {}", task),
        ResetReason::Unknown => info!("Reset due to unknown reason"),
        ResetReason::None => {}
    }

    // Spawn the sensor tasks
    spawn_sensor_task(
        &spawner,
        resources.i2cdev,
        sender,
        mmc_sender,
        start_time,
        watchdog,
    );
    trace!("Sensor task spawned");

    // Spawn core 1
    #[cfg(feature = "dual-core")]
    {
        const STACK_SIZE: usize = 128 * 1024; // 128 KB stack for core 1
        static CORE1_STACK: StaticCell<Stack<STACK_SIZE>> = StaticCell::new();
        static CORE1_EXECUTOR: StaticCell<Executor> = StaticCell::new();
        let stack = CORE1_STACK.init(Stack::new());
        spawn_core1(p.CORE1, stack, move || {
            let exec = CORE1_EXECUTOR.init(Executor::new());
            exec.run({
                move |spawner| {
                    // Spawn the local storage task
                    localstor::local_storage_init(spawner, resources.mmc, mmc_receiver);
                    // Spawn the Wi-Fi task
                    spawner.spawn(
                        wifi::wifi_task(
                            watchdog,
                            spawner,
                            resources.wifi,
                            sender,
                            receiver,
                            rq_recv,
                            rp_send,
                        )
                        .unwrap(),
                    );
                }
            })
        });
    }
    #[cfg(not(feature = "dual-core"))]
    {
        // Spawn the local storage task
        localstor::local_storage_init(spawner, resources.mmc, mmc_receiver);
        // Spawn the Wi-Fi task
        spawner.spawn(
            wifi::wifi_task(
                watchdog,
                spawner,
                resources.wifi,
                sender,
                receiver,
                rq_recv,
                rp_send,
            )
            .unwrap(),
        );
    }
    trace!("Wi-Fi task spawned");
    // BOOTSEL must be polled from core 0: it switches the radio between WiFi and BLE
    spawner.spawn(bootsel::bootsel_task(resources.bootsel.bootsel).unwrap());
    spawner.spawn(watchdog_task(watchdogrunner).unwrap());
}

#[embassy_executor::task]
async fn watchdog_task(wdrunner: WatchdogRunner) {
    wdrunner.run().await;
}

#[cfg(feature = "cpuinfo")]
#[embassy_executor::task]
async fn cpu_report() {
    embassy_cpu_usage::init();
    loop {
        embassy_time::Timer::after_secs(1).await;
        embassy_cpu_usage::log(true); // print via defmt, reset the window
    }
}
