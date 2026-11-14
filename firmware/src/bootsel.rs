//! BOOTSEL button support for the RP2350.
//!
//! The BOOTSEL button pulls the QSPI flash chip-select line low through a 1 kΩ
//! resistor. To read it, the CS pad's output driver is briefly disabled, the line
//! is allowed to settle, and its input level is sampled. Flash must not be
//! accessed during that window, so the read runs from RAM with interrupts
//! disabled, CORE1 paused and no DMA reading from flash.
//!
//! `embassy_rp::bootsel` only supports the RP2040; on the RP2350 the SS pad sits
//! at a different index in `IO_QSPI` and the GPIO_CTRL override fields moved.

use embassy_rp::{Peri, pac, peripherals::BOOTSEL};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use embassy_time::{Duration, Ticker};

/// Signalled once for every debounced press of the BOOTSEL button.
pub static BOOTSEL_PRESSED: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Polling period of the BOOTSEL button
const POLL_PERIOD: Duration = Duration::from_millis(20);
/// Number of consecutive identical samples required to accept a state change
const DEBOUNCE_SAMPLES: u8 = 3;

/// Reads the BOOTSEL button. Returns true if the button is pressed.
///
/// Must be called from CORE0. The read takes ~4000 CPU cycles with interrupts
/// disabled and CORE1 paused.
pub fn is_bootsel_pressed_unchecked(_p: &mut Peri<'_, BOOTSEL>) -> bool {
    // Make sure we're running on CORE0
    assert_eq!(
        pac::SIO.cpuid().read(),
        0,
        "BOOTSEL must be read from CORE0"
    );

    // Keep CORE1 from executing from flash while CS is floating
    embassy_rp::multicore::pause_core1();
    let status = critical_section::with(|_| {
        // Wait for any DMA reading from flash to finish
        const SRAM_LOWER: u32 = 0x2000_0000;
        for n in 0..DMA_CHANNELS {
            let ch = pac::DMA.ch(n);
            while ch.read_addr().read() < SRAM_LOWER && ch.ctrl_trig().read().busy() {}
        }
        // Wait for completion of any background XIP stream reads
        while pac::XIP_CTRL.stream_ctr().read().0 > 0 {}
        // SAFETY: flash is idle and stays idle: interrupts are off, CORE1 is paused
        // and no DMA channel is reading from flash.
        unsafe { read_cs_status() }
    });
    embassy_rp::multicore::resume_core1();

    // BOOTSEL is active low
    !pac::io::regs::GpioStatus(status).infrompad()
}

/// Number of DMA channels on the RP2350
const DMA_CHANNELS: usize = 16;

/// Temporarily floats the QSPI SS pad and returns its GPIO_STATUS register.
///
/// # Safety
///
/// Flash must be idle and must remain idle for the duration of the call. This
/// function lives in RAM and uses inline asm only, so it does not touch flash.
#[inline(never)]
#[unsafe(link_section = ".data.ram_func")]
unsafe fn read_cs_status() -> u32 {
    let result: u32;
    // GPIO_CTRL.OEOVER = DISABLE (0b10 at bits [15:14] on the RP2350). Also used as
    // the delay loop counter.
    let magic: u32 = 0x8000;
    unsafe {
        core::arch::asm!(
            ".equiv GPIO_STATUS, 0x0",
            ".equiv GPIO_CTRL,   0x4",

            "ldr {orig_ctrl}, [{cs_gpio}, $GPIO_CTRL]",

            // Disable CS's output drive so the BOOTSEL pull-down can win...
            "str {val}, [{cs_gpio}, $GPIO_CTRL]",

            // ...wait for the line to settle (~4000 cycles)...
            "2:",
            "subs {val}, #8",
            "bne 2b",

            // ...sample the pad...
            "ldr {val}, [{cs_gpio}, $GPIO_STATUS]",

            // ...and restore CS so XIP can continue
            "str {orig_ctrl}, [{cs_gpio}, $GPIO_CTRL]",

            // On the RP2350, IO_QSPI starts with USBPHY_DP and USBPHY_DM, followed
            // by QSPI_SCLK and QSPI_SS: the SS pad is at index 3.
            cs_gpio = in(reg) pac::IO_QSPI.gpio(3).as_ptr(),
            orig_ctrl = out(reg) _,
            val = inout(reg) magic => result,
            options(nostack),
        );
    }
    result
}

/// Polls the BOOTSEL button and signals [`BOOTSEL_PRESSED`] on each debounced
/// press. Must be spawned on CORE0.
#[embassy_executor::task]
pub async fn bootsel_task(mut bootsel: Peri<'static, BOOTSEL>) -> ! {
    let mut ticker = Ticker::every(POLL_PERIOD);
    let mut pressed = false;
    let mut count = 0u8;
    loop {
        ticker.next().await;
        if is_bootsel_pressed_unchecked(&mut bootsel) == pressed {
            count = 0;
            continue;
        }
        count += 1;
        if count >= DEBOUNCE_SAMPLES {
            count = 0;
            pressed = !pressed;
            if pressed {
                defmt::debug!("BOOTSEL pressed");
                BOOTSEL_PRESSED.signal(());
            }
        }
    }
}
