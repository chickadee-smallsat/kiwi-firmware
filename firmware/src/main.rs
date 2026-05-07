#![no_std]
#![no_main]

use kiwi_firmware_base::{
    DataRateReplyChannel, DataRateRequestChannel, MeasurementChannel, DEFAULT_DEVICE_NAME,
    DEFAULT_WIFI_HOST_AP,
};
use embassy_sync::channel::Channel;
// Re-export lib types so submodules can use `crate::` to access them.
pub use kiwi_firmware_base::{
    DataRateReplyReceiver, DataRateReplySender, DataRateRequestReceiver, DataRateRequestSender,
    MeasurementReceiver, MeasurementSender, MEASUREMENT_CHANNEL_SIZE,
};
use defmt::*;
use embassy_executor::{Executor, Spawner};
use embassy_rp::multicore::{Stack, spawn_core1};
pub use embassy_task_watchdog::embassy_rp::TaskWatchdog;
use embassy_task_watchdog::{
    ResetReason, WatchdogConfig, create_watchdog, embassy_rp::WatchdogRunner,
};
use embassy_time::{Duration, Instant, Timer};
use static_cell::StaticCell;

use {defmt_rtt as _, panic_probe as _};

mod resources;
mod sensor_tasks;
mod usb;
mod wifi;
mod wifi_fw_consts;

use crate::resources::ConfigUpdateDev;
use crate::{
    resources::{AssignedResources, I2cDev, UsbConfDev, WifiPins},
    sensor_tasks::spawn_sensor_task,
};

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let start_time = Instant::now();
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

    info!("Kiwi Demo FW starting up...");
    match watchdog.reset_reason().await {
        ResetReason::Forced(reason) => info!("Forced reset, reason: {}", reason),
        ResetReason::TimedOut(task) => info!("Task timed out: {}", task),
        ResetReason::Unknown => info!("Reset due to unknown reason"),
        ResetReason::None => {}
    }
    // Set up flash memory
    // add some delay to give an attached debug probe time to parse the
    // defmt RTT header. Reading that header might touch flash memory, which
    // interferes with flash write operations.
    // https://github.com/knurling-rs/defmt/pull/683
    Timer::after_millis(10).await;

    let receiver = channel.receiver();
    let sender = channel.sender();

    // Spawn the sensor tasks
    spawn_sensor_task(&spawner, resources.i2cdev, sender, start_time, watchdog);
    trace!("Sensor task spawned");

    // Spawn core 1
    const STACK_SIZE: usize = 128 * 1024; // 128 KB stack for core 1
    static CORE1_STACK: StaticCell<Stack<STACK_SIZE>> = StaticCell::new();
    static CORE1_EXECUTOR: StaticCell<Executor> = StaticCell::new();
    let stack = CORE1_STACK.init(Stack::new());
    spawn_core1(p.CORE1, stack, move || {
        let exec = CORE1_EXECUTOR.init(Executor::new());
        exec.run({
            move |spawner| {
                // Spawn the Wi-Fi task
                spawner
                    .spawn(wifi::wifi_task(
                        watchdog,
                        spawner,
                        resources.wifi,
                        sender,
                        receiver,
                        rq_recv,
                        rp_send,
                    ))
                    .unwrap();
            }
        })
    });
    trace!("Wi-Fi task spawned");
    // Spawn the USB task
    usb::usb_task(
        &spawner,
        watchdog,
        resources.usbconfig,
        resources.confdev,
        rq_send,
        rp_recv,
    );
    spawner.must_spawn(watchdog_task(watchdogrunner));
}

#[embassy_executor::task]
async fn watchdog_task(wdrunner: WatchdogRunner) {
    wdrunner.run().await;
}
