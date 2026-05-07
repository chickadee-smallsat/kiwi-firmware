#[cfg(feature = "sensor-humi")]
use bme680_rs::{ohm, percent};
use defmt::*;

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_executor::Spawner;
use embassy_rp::{
    gpio::Input,
    i2c::{Async as I2cAsync, Config, I2c},
    peripherals::I2C0,
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_task_watchdog::embassy_rp::TaskWatchdog;
use embassy_time::{Duration, Instant};
use kiwi_measurements::{CommonMeasurement, SingleMeasurement};

#[cfg(feature = "sensor-baro")]
use libm::powf;
use static_cell::StaticCell;
#[allow(unused_imports)]
use uom::si::{
    f32::{Length, Pressure, ThermodynamicTemperature},
    length::foot,
    magnetic_flux_density::gauss,
    pressure::hectopascal,
    thermodynamic_temperature::degree_celsius,
};

use crate::{
    MeasurementSender,
    resources::{I2cDev, IrqHandlers},
};

type SharedI2cBus = I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, I2cAsync>>;

pub fn spawn_sensor_task(
    spawner: &Spawner,
    dev: I2cDev,
    channel: MeasurementSender,
    reftime: Instant,
    watchdog: TaskWatchdog,
) {
    let i2c_config = Config::default();
    let i2c = Mutex::new(I2c::new_async(
        dev.i2c,
        dev.scl,
        dev.sda,
        IrqHandlers,
        i2c_config,
    ));

    static I2C_BUS: StaticCell<Mutex<NoopRawMutex, I2c<I2C0, I2cAsync>>> = StaticCell::new();
    let i2c_bus = I2C_BUS.init(i2c);

    #[cfg(feature = "sensor-mag")]
    {
        static MMCIRQ: StaticCell<Input<'static>> = StaticCell::new();
        let mmc_i2c = I2cDevice::new(i2c_bus);
        let mmcirq = MMCIRQ.init(Input::new(dev.mmcirq, embassy_rp::gpio::Pull::None));
        unwrap!(spawner.spawn(magnetometer_task(watchdog, mmc_i2c, mmcirq, channel, reftime)));
    }

    #[cfg(feature = "sensor-imu")]
    {
        static IMUIRQ: StaticCell<Input<'static>> = StaticCell::new();
        let imu_i2c = I2cDevice::new(i2c_bus);
        let imuirq = IMUIRQ.init(Input::new(dev.imuirq, embassy_rp::gpio::Pull::None));
        unwrap!(spawner.spawn(imu_task(watchdog, imu_i2c, imuirq, channel, reftime)));
    }

    #[cfg(feature = "sensor-baro")]
    {
        static BAROIRQ: StaticCell<Input<'static>> = StaticCell::new();
        let baro_i2c = I2cDevice::new(i2c_bus);
        let baroirq = BAROIRQ.init(Input::new(dev.baroirq, embassy_rp::gpio::Pull::None));
        unwrap!(spawner.spawn(baro_task(watchdog, baro_i2c, baroirq, channel, reftime)));
    }

    #[cfg(feature = "sensor-humi")]
    {
        let humi_i2c = I2cDevice::new(i2c_bus);
        unwrap!(spawner.spawn(humi_gas_task(watchdog, humi_i2c, channel, reftime)));
    }
}

/// Task for handling the MMC5983MA magnetometer.
/// Uses interrupt-driven measurements to read data whenever the sensor indicates new data is available.
/// Feeds the watchdog on each measurement to ensure the system remains responsive.
/// If the sensor initialization or measurement loop encounters an error, it logs the error and exits the task.
#[cfg(feature = "sensor-mag")]
#[embassy_task_watchdog::task(timeout = Duration::from_secs(5), fallible = true)]
pub async fn magnetometer_task(
    watchdog: TaskWatchdog,
    i2c: SharedI2cBus,
    irq: &'static mut Input<'static>,
    sender: MeasurementSender,
    tref: Instant,
) {
    use mmc5983ma::{
        ContinuousMeasurementFreq, DEFAULT_I2C_ADDRESS, Mmc5983Async, Mmc5983ConfigBuilder,
    };
    debug!("Initializing MMC5983MA...");
    let mut mmc = Mmc5983Async::<_, _, NoopRawMutex>::new_with_i2c(
        i2c,
        DEFAULT_I2C_ADDRESS,
        Mmc5983ConfigBuilder::default()
            .frequency(ContinuousMeasurementFreq::Hz10)
            .set_interval(mmc5983ma::PeriodicSetInterval::Per500)
            .build(),
        embassy_time::Delay,
    );
    if let Err(e) = mmc.init().await {
        error!("Failed to initialize MMC5983MA: {:?}", e);
        return;
    }
    debug!("MMC5983MA initialized");
    if let Err(e) = mmc.remove_offset().await {
        error!("Failed to remove MMC5983MA offset: {:?}", e);
        return;
    }
    debug!("MMC5983MA offset removed");
    let temp = mmc.temperature().await.unwrap();
    debug!(
        "Temperature: {}°C",
        ThermodynamicTemperature::from(temp).get::<degree_celsius>()
    );
    let mag = mmc
        .measure()
        .await
        .map_err(|e| error!("Failed to take initial measurement: {:?}", e))
        .unwrap();
    debug!("Magnetometer reading: {}", mag);
    // Take a measurement to ensure the sensor is working
    debug!("MMC5983MA magnetometer starting continuous acquisition");
    if let Err(e) = mmc
        .start(|mmc| async {
            let mut count = 0;
            loop {
                watchdog.feed().await;
                irq.wait_for_any_edge().await;
                let now = Instant::now();
                let mes = mmc.measure().await?;
                let x = mes.x.get::<gauss>() * 1e3;
                let y = mes.y.get::<gauss>() * 1e3;
                let z = mes.z.get::<gauss>() * 1e3;
                count += 1;
                if count % 25 == 0 {
                    debug!("Magnetometer reading: {}", mes);
                }
                {
                    if !sender.is_full() {
                        let measurement = CommonMeasurement::Mag(x, y, z);
                        sender
                            .send(SingleMeasurement {
                                measurement,
                                timestamp: now.duration_since(tref).as_micros(),
                            })
                            .await;
                    }
                }
            }
        })
        .await
    {
        error!("MMC5983MA measurement loop exited with error: {:?}", e);
    }
}

#[cfg(feature = "sensor-imu")]
#[embassy_task_watchdog::task(timeout = Duration::from_secs(1), fallible = true)]
pub async fn imu_task(
    watchdog: TaskWatchdog,
    i2c: SharedI2cBus,
    irq: &'static mut Input<'static>,
    sender: MeasurementSender,
    tref: Instant,
) {
    use bmi323_rs::{Bmi323Async, Bmi323Config, GyroRange};
    let mut imu = Bmi323Async::<_, _, NoopRawMutex>::new_with_i2c(
        i2c,
        bmi323_rs::DEFAULT_I2C_ADDRESS,
        Bmi323Config::default()
            .with_accel_mode(bmi323_rs::AccelMode::HighPerformance)
            .with_gyro_mode(bmi323_rs::GyroMode::HighPerformance)
            .with_gyro_range(GyroRange::Auto)
            .with_accel_odr(
                if cfg!(feature = "imu-accel-odr-1600hz")     { bmi323_rs::OutputDataRate::Hz1600 }
                else if cfg!(feature = "imu-accel-odr-800hz") { bmi323_rs::OutputDataRate::Hz800 }
                else if cfg!(feature = "imu-accel-odr-400hz") { bmi323_rs::OutputDataRate::Hz400 }
                else if cfg!(feature = "imu-accel-odr-200hz") { bmi323_rs::OutputDataRate::Hz200 }
                else if cfg!(feature = "imu-accel-odr-100hz") { bmi323_rs::OutputDataRate::Hz100 }
                else if cfg!(feature = "imu-accel-odr-25hz")  { bmi323_rs::OutputDataRate::Hz25 }
                else if cfg!(feature = "imu-accel-odr-12hz5") { bmi323_rs::OutputDataRate::Hz12_5 }
                else if cfg!(feature = "imu-accel-odr-6hz25") { bmi323_rs::OutputDataRate::Hz6_25 }
                else if cfg!(feature = "imu-accel-odr-3hz125") { bmi323_rs::OutputDataRate::Hz3_125 }
                else if cfg!(feature = "imu-accel-odr-1hz56") { bmi323_rs::OutputDataRate::Hz1_5625 }
                else if cfg!(feature = "imu-accel-odr-0hz78") { bmi323_rs::OutputDataRate::Hz0_78125 }
                else { bmi323_rs::OutputDataRate::Hz50 } // default: imu-accel-odr-50hz
            )
            .with_gyro_odr(
                if cfg!(feature = "imu-gyro-odr-1600hz")     { bmi323_rs::OutputDataRate::Hz1600 }
                else if cfg!(feature = "imu-gyro-odr-800hz") { bmi323_rs::OutputDataRate::Hz800 }
                else if cfg!(feature = "imu-gyro-odr-400hz") { bmi323_rs::OutputDataRate::Hz400 }
                else if cfg!(feature = "imu-gyro-odr-200hz") { bmi323_rs::OutputDataRate::Hz200 }
                else if cfg!(feature = "imu-gyro-odr-100hz") { bmi323_rs::OutputDataRate::Hz100 }
                else if cfg!(feature = "imu-gyro-odr-25hz")  { bmi323_rs::OutputDataRate::Hz25 }
                else if cfg!(feature = "imu-gyro-odr-12hz5") { bmi323_rs::OutputDataRate::Hz12_5 }
                else if cfg!(feature = "imu-gyro-odr-6hz25") { bmi323_rs::OutputDataRate::Hz6_25 }
                else if cfg!(feature = "imu-gyro-odr-3hz125") { bmi323_rs::OutputDataRate::Hz3_125 }
                else if cfg!(feature = "imu-gyro-odr-1hz56") { bmi323_rs::OutputDataRate::Hz1_5625 }
                else if cfg!(feature = "imu-gyro-odr-0hz78") { bmi323_rs::OutputDataRate::Hz0_78125 }
                else { bmi323_rs::OutputDataRate::Hz50 } // default: imu-gyro-odr-50hz
            )
            .with_acc_irq(bmi323_rs::IrqMap::Int2)
            .with_gyro_irq(bmi323_rs::IrqMap::Int2)
            .with_temp_irq(bmi323_rs::IrqMap::Int2)
            .with_auto_range_hysteresis(core::time::Duration::from_millis(500)),
        embassy_time::Delay,
    );
    if let Err(e) = imu.init().await {
        error!("Failed to initialize BMI323: {:?}", e);
        return;
    }
    trace!("BMI323 Inertial Measurement Unit initialized");
    imu.calibrate(bmi323_rs::SelfCalibrateType::Both)
        .await
        .unwrap();
    // trace!("bmi323_rs calibrated");
    debug!("BMI323 IMU starting measurements");
    let mut count = 0;
    if let Err(e) = imu.start(|imu| async {
        loop {
            watchdog.feed().await;
            irq.wait_for_falling_edge().await;
            let now = Instant::now();
            // Handle interrupt
            count += 1;
            if let Ok(data) = imu.measure().await {
                let accel = if let Some(accel) = data.accel {
                    let (ax, ay, az) = accel.float();
                    let mag_a = libm::sqrtf(ax * ax + ay * ay + az * az);
                    let theta_a = libm::acosf(az / mag_a) * 180.0 / core::f32::consts::PI;
                    let phi_a = libm::atan2f(ay, ax) * 180.0 / core::f32::consts::PI;
                    trace!(
                        "Accel: x={}g y={}g z={}g, Field magnitude: {}, Field angles: θ={}° φ={}°",
                        ax, ay, az, mag_a, theta_a, phi_a
                    );
                    if count % 25 == 0 {
                        debug!(
                            "Accel: x={}g y={}g z={}g, Field magnitude: {}, Field angles: θ={}° φ={}°",
                            ax, ay, az, mag_a, theta_a, phi_a
                        );
                    }
                    Some((ax, ay, az))
                } else {
                    None
                };
                let gyr = if let Some(gyro) = data.gyro {
                    let (gx, gy, gz) = gyro.float();
                    trace!("Gyro: x={}dps y={}dps z={}dps", gx, gy, gz);
                    if count % 25 == 0 {
                        debug!("Gyro: x={}dps y={}dps z={}dps", gx, gy, gz);
                    }
                    Some((gx, gy, gz))
                } else {
                    None
                };
                if let Some(temp) = data.temp {
                    trace!("Temperature: {}°C", temp.celcius());
                    if count % 25 == 0 {
                        debug!("IMU Temperature: {}°C", temp.celcius());
                    }
                }
                {
                    if !sender.is_full() {
                        if let Some((ax, ay, az)) = accel {
                            let measurement = CommonMeasurement::Accel(ax, ay, az);
                            sender
                                .send(SingleMeasurement {
                                    measurement,
                                    timestamp: now.duration_since(tref).as_micros(),
                                })
                                .await;
                        }
                        if let Some((gx, gy, gz)) = gyr {
                            let measurement = CommonMeasurement::Gyro(gx, gy, gz);
                            sender
                                .send(SingleMeasurement {
                                    measurement,
                                    timestamp: now.duration_since(tref).as_micros(),
                                })
                                .await;
                        }
                    }
                }
            }
        }
    })
    .await {
        error!("BMI323 measurement loop exited with error: {:?}", e);
    }
}

#[cfg(feature = "sensor-baro")]
#[embassy_task_watchdog::task(timeout = Duration::from_secs(1), fallible = true)]
pub async fn baro_task(
    watchdog: TaskWatchdog,
    i2c: SharedI2cBus,
    #[allow(unused_variables)] irq: &'static mut Input<'static>,
    sender: MeasurementSender,
    tref: Instant,
) {
    {
        use embassy_time::Delay;
        use ms5607_rs::AsyncInterface as _;
        let mut ticker = embassy_time::Ticker::every(Duration::from_millis(100));
        const I2C_ADDR: u8 = 0x77; // 0x76;
        let mut ms5607 = ms5607_rs::Ms5607::new_i2c(
            i2c,
            I2C_ADDR.try_into().unwrap(),
            Delay,
            ms5607_rs::Oversampling::Osr4096,
        );
        debug!("Initializing MS5607...");
        ms5607.init().await.expect("Failed to initialize MS5607");
        debug!("MS5607 initialized successfully!");
        loop {
            watchdog.feed().await;
            let measurement = ms5607.read().await.expect("Failed to read from MS5607");
            let now = Instant::now();
            let altitude = calculate_altitude(measurement.pressure, Length::new::<foot>(0.0));
            trace!(
                "Temperature: {} °C, Pressure: {} hPa, Altitude: {} ft",
                measurement.temperature.get::<degree_celsius>(),
                measurement.pressure.get::<hectopascal>(),
                altitude.get::<foot>(),
            );
            if !sender.is_full() {
                let measurement = CommonMeasurement::Baro(
                    measurement.temperature.get::<degree_celsius>(),
                    measurement.pressure.get::<hectopascal>(),
                    altitude.get::<foot>(),
                );
                sender
                    .send(SingleMeasurement {
                        measurement,
                        timestamp: now.duration_since(tref).as_micros(),
                    })
                    .await;
            }
            ticker.next().await;
        }
    }
}

#[cfg(feature = "sensor-humi")]
use bme680_rs::{Bme680, Bme680Address, Config as Bme680Config};

#[cfg(feature = "sensor-humi")]
#[embassy_task_watchdog::task(timeout = Duration::from_secs((Bme680Config::default().measure_duration_ms() as u64).div_ceil(1000) + 1), fallible = true)]
async fn humi_gas_task(
    watchdog: TaskWatchdog,
    i2c: SharedI2cBus,
    sender: MeasurementSender,
    tref: Instant,
) {
    let mut bme =
        Bme680::<_, embassy_time::Delay>::new(i2c, Bme680Address::AddrLow, embassy_time::Delay);
    bme.init(Bme680Config::default()).await.unwrap();
    let mut ticker = embassy_time::Ticker::every(Duration::from_secs(1));
    loop {
        watchdog.feed().await;
        match bme.measure().await {
            Ok(m) => {
                trace!(
                    "Temperature: {} °C  Pressure: {} hPa  Humidity: {} %  Gas Resistance: {} Ω  Gas Valid: {}",
                    m.temperature.get::<degree_celsius>(),
                    m.pressure.get::<hectopascal>(),
                    m.humidity.get::<percent>(),
                    m.gas_resistance.get::<ohm>(),
                    m.gas_valid,
                );
                if !sender.is_full() {
                    let measurement = CommonMeasurement::Humi(
                        m.temperature.get::<degree_celsius>(),
                        m.humidity.get::<percent>(),
                        m.gas_resistance.get::<ohm>(),
                    );
                    sender
                        .send(SingleMeasurement {
                            measurement,
                            timestamp: Instant::now().duration_since(tref).as_micros(),
                        })
                        .await;
                }
            }
            Err(bme680_rs::Error::MeasurementTimedOut) => {
                error!("BME680 measurement timed out (new_data never set)")
            }
            Err(bme680_rs::Error::InvalidChipId) => error!("BME680 invalid chip ID"),
            Err(bme680_rs::Error::I2c(_)) => error!("BME680 I2C bus error during measurement"),
        }
        ticker.next().await;
    }
}

#[cfg(feature = "sensor-baro")]
/// Calculate the altitude based on the pressure, sea level pressure, and the reference altitude.
///
/// The altitude is calculating following the [NOAA formula](https://www.weather.gov/media/epz/wxcalc/pressureAltitude.pdf).
fn calculate_altitude(pressure: Pressure, altitude_reference: Length) -> Length {
    let sea_level = Pressure::new::<hectopascal>(1013.25);
    let above_sea_level =
        Length::new::<foot>(145366.45 * (1.0 - powf((pressure / sea_level).value, 0.190284)));

    above_sea_level - altitude_reference
}
