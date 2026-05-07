#![no_std]
//! Shared types and constants for the AP UDP demo firmware.

use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex,
    channel::{Channel, Receiver, Sender},
};
use kiwi_measurements::SingleMeasurement;

/// Default SSID for the Wi-Fi Access Point (no password)
pub const DEFAULT_WIFI_HOST_AP: &str = "kiwi-ap";
/// Default device name
pub const DEFAULT_DEVICE_NAME: &str = "kiwi#0001";
/// Size of the channel buffer for transmitting `SingleMeasurement` data between tasks.
pub const MEASUREMENT_CHANNEL_SIZE: usize = 32;

/// Type alias for a channel that transmits `SingleMeasurement` data between tasks, using a critical section mutex for synchronization and a defined buffer size.
pub type MeasurementChannel =
    Channel<CriticalSectionRawMutex, SingleMeasurement, MEASUREMENT_CHANNEL_SIZE>;
/// Type alias for the sender half of the `MeasurementChannel`, allowing tasks to send `SingleMeasurement` data.
pub type MeasurementSender =
    Sender<'static, CriticalSectionRawMutex, SingleMeasurement, MEASUREMENT_CHANNEL_SIZE>;
/// Type alias for the receiver half of the `MeasurementChannel`, allowing tasks to receive `SingleMeasurement` data.
pub type MeasurementReceiver =
    Receiver<'static, CriticalSectionRawMutex, SingleMeasurement, MEASUREMENT_CHANNEL_SIZE>;
/// Type alias for a channel that transmits data rate requests, using a critical section mutex for synchronization and a buffer size of 1.
pub type DataRateRequestChannel = Channel<CriticalSectionRawMutex, (), 1>;
/// Type alias for the sender half of the `DataRateRequestChannel`, allowing tasks to send data rate requests.
pub type DataRateRequestSender = Sender<'static, CriticalSectionRawMutex, (), 1>;
/// Type alias for the receiver half of the `DataRateRequestChannel`, allowing tasks to receive data rate requests.
pub type DataRateRequestReceiver = Receiver<'static, CriticalSectionRawMutex, (), 1>;
/// Type alias for a channel that transmits data rate replies, using a critical section mutex for synchronization and a buffer size of 1.
pub type DataRateReplyChannel =
    Channel<CriticalSectionRawMutex, Option<(f32, &'static str, f32)>, 1>;
/// Type alias for the sender half of the `DataRateReplyChannel`, allowing tasks to send data rate replies.
pub type DataRateReplySender =
    Sender<'static, CriticalSectionRawMutex, Option<(f32, &'static str, f32)>, 1>;
/// Type alias for the receiver half of the `DataRateReplyChannel`, allowing tasks to receive data rate replies.
pub type DataRateReplyReceiver =
    Receiver<'static, CriticalSectionRawMutex, Option<(f32, &'static str, f32)>, 1>;
