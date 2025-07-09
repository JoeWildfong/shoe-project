#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU8, Ordering};

use bt_hci::controller::ExternalController;
use defmt::info;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use esp_hal::analog::adc::{Adc, AdcConfig, Attenuation};
use esp_hal::ledc::channel::ChannelIFace;
use esp_hal::ledc::timer::TimerIFace;
use esp_hal::ledc::{channel, timer, LSGlobalClkSource, LowSpeed};
use esp_hal::time::Rate;
use esp_hal::{clock::CpuClock, ledc::Ledc};
use esp_hal::timer::systimer::SystemTimer;
use esp_hal::timer::timg::TimerGroup;
use esp_wifi::{ble::controller::BleConnector, EspWifiController};
use l298_motor::in_switch::L298Channel;

use {esp_backtrace as _, esp_println as _};

extern crate alloc;

mod ble;

static INCOMING_COMMANDS: Signal<CriticalSectionRawMutex, i8> = Signal::new();
static BATTERY: AtomicU8 = AtomicU8::new(100);

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    // generator version: 0.3.1

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(size: 72 * 1024);

    let timer0 = SystemTimer::new(peripherals.SYSTIMER);
    esp_hal_embassy::init(timer0.alarm0);

    info!("Embassy initialized!");

    spawner.spawn(battery(peripherals.ADC1, peripherals.GPIO4)).unwrap();

    let timer1 = TimerGroup::new(peripherals.TIMG0);
    let init = esp_wifi::init(
        timer1.timer0,
        esp_hal::rng::Rng::new(peripherals.RNG),
        peripherals.RADIO_CLK,
    )
    .unwrap();

    spawner.spawn(ble(init, peripherals.BT, &INCOMING_COMMANDS)).unwrap();

    let mut ledc = Ledc::new(peripherals.LEDC);
    ledc.set_global_slow_clock(LSGlobalClkSource::APBClk);

    let mut lstimer0 = ledc.timer::<LowSpeed>(timer::Number::Timer0);
    lstimer0
        .configure(timer::config::Config {
            duty: timer::config::Duty::Duty5Bit,
            clock_source: timer::LSClockSource::APBClk,
            frequency: Rate::from_hz(50),
        })
        .unwrap();

    let mut channel0 = ledc.channel(channel::Number::Channel0, peripherals.GPIO2);
    channel0
        .configure(channel::config::Config {
            timer: &lstimer0,
            duty_pct: 0,
            pin_config: channel::config::PinConfig::PushPull,
        })
        .unwrap();
    let mut channel1 = ledc.channel(channel::Number::Channel1, peripherals.GPIO3);
    channel1
        .configure(channel::config::Config {
            timer: &lstimer0,
            duty_pct: 0,
            pin_config: channel::config::PinConfig::PushPull,
        })
        .unwrap();

    let mut motor = L298Channel::new(channel0, channel1);

    loop {
        let v = INCOMING_COMMANDS.wait().await;
        let percent = match v {
            -1 => Some(100),
            0 => Some(0),
            1 => Some(-100),
            a => {
                info!("ignoring unrecognized command {}", a);
                None
            }
        };
        if let Some(v) = percent {
            info!("motor set to {}%", v);
            motor.percent(v);
        }
    }
}

#[embassy_executor::task]
async fn ble(init: EspWifiController<'static>, bt: esp_hal::peripherals::BT, signal: &'static Signal<CriticalSectionRawMutex, i8>) {
    let connector = BleConnector::new(&init, bt);
    let controller: ExternalController<_, 20> = ExternalController::new(connector);
    ble::run_ble(controller, |v| signal.signal(v)).await
}

#[embassy_executor::task]
async fn battery(adc: esp_hal::peripherals::ADC1, gpio: esp_hal::gpio::GpioPin<4>) {
    const MIN_BATTERY: u32 = 2620;
    const MAX_BATTERY: u32 = 3440;
    let mut config = AdcConfig::new();
    let mut pin = config.enable_pin(gpio, Attenuation::_11dB);
    let mut adc = Adc::new(adc, config).into_async();
    loop {
        let reading = u32::from(adc.read_oneshot(&mut pin).await);
        let percent = 100 * (reading - MIN_BATTERY) / (MAX_BATTERY - MIN_BATTERY);
        let percent = (percent as u8).min(100);
        BATTERY.store(percent, Ordering::Relaxed);
        info!("[batt] {}% (raw {})", percent, reading);
        embassy_time::Timer::after_secs(60).await;
    }
}

pub fn read_battery() -> u8 {
    BATTERY.load(Ordering::Relaxed)
}
