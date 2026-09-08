#![no_std]
#![no_main]

mod progress;

use core::ptr;
use cortex_m::interrupt::free;
use cortex_m::peripheral::{syst::SystClkSource, SYST};
use cortex_m_rt::entry;
use embedded_hal::digital::v2::InputPin;
use embedded_hal::serial::Write as SerialWrite;
use panic_halt as _;
use progress::{Progress, LED_COUNT};
use stm32f0xx_hal::gpio::{
    gpioa::PA0, gpioa::PA1, gpioa::PA10, gpioa::PA9, Alternate, Input, PullUp, AF1,
};
use stm32f0xx_hal::pac;
use stm32f0xx_hal::prelude::*;
use stm32f0xx_hal::rcc::HSEBypassMode;
use stm32f0xx_hal::serial::Serial;
use stm32f0xx_hal::time::Hertz;
use stm32f0xx_hal::watchdog::Watchdog;

const UPDATE_DELAY: u32 = 100_000;
const WS_RESET_DELAY: u32 = 500_000;
const WS_T0L: u32 = 10;
const WS_T1H: u32 = 10;
const WS_T1L: u32 = 0;
const HEARTBEAT_PIN: u8 = 4;
const HEARTBEAT_TOGGLE_FRAMES: u8 = 4;
const GPIOA_BSRR: *mut u32 = 0x4800_0018 as *mut u32;
const RELAY_PIN: u8 = 7;
const DFPLAYER_USE_MP3_FOLDER: bool = true;
const START_TRACK: u8 = 1;
const FAILED_START_MIN_TRACK: u8 = 2;
const FAILED_START_TRACKS: u8 = 3;
const RUN_TRACK: u8 = 5;
const SUCCESS_START_STEPS: u16 = 85;
const RUN_TRACK_REPEAT_STEPS: u16 = 500;
const FAILED_TRACK_MIN_STEPS: u16 = 360;
const FAILED_TRACK_RANDOM_STEPS: u16 = 320;
const DFPLAYER_WRITE_TIMEOUT: u16 = 30_000;
const DFPLAYER_BOOT_DELAY_STEPS: u16 = 80;
const DFPLAYER_START_RETRY_STEPS: u16 = 80;
const DFPLAYER_VOLUME: u8 = 24;

type ModePin = PA0<Input<PullUp>>;
type NoisePin = PA1<Input<PullUp>>;
type TxPin = PA9<Alternate<AF1>>;
type RxPin = PA10<Alternate<AF1>>;
type DfSerial = Serial<pac::USART1, TxPin, RxPin>;

#[derive(Copy, Clone, PartialEq, Eq)]
enum LampMode {
    Off,
    Starting,
    On,
}

struct Ws2815 {
    set_mask: u32,
    clear_mask: u32,
}

impl Ws2815 {
    fn new(pin_number: u8) -> Self {
        let bit = 1u32 << pin_number;
        Self {
            set_mask: bit,
            clear_mask: bit << 16,
        }
    }

    #[inline(always)]
    fn send_reset(&mut self) {
        self.set_low();
        cortex_m::asm::delay(WS_RESET_DELAY);
    }

    #[inline(always)]
    fn write_mask(&self, mask: u32) {
        unsafe {
            ptr::write_volatile(GPIOA_BSRR, mask);
        }
    }

    #[inline(always)]
    fn set_high(&self) {
        self.write_mask(self.set_mask);
    }

    #[inline(always)]
    fn set_low(&self) {
        self.write_mask(self.clear_mask);
    }

    #[inline(always)]
    fn write_zero(&self) {
        self.set_high();
        self.set_low();
        cortex_m::asm::delay(WS_T0L);
    }

    #[inline(always)]
    fn write_one(&self) {
        self.set_high();
        cortex_m::asm::delay(WS_T1H);
        self.set_low();
        cortex_m::asm::delay(WS_T1L);
    }

    #[inline(always)]
    fn write_byte(&mut self, byte: u8) {
        for bit in (0..8).rev() {
            if (byte & (1 << bit)) != 0 {
                self.write_one();
            } else {
                self.write_zero();
            }
        }
    }

    fn write_frame(&mut self, colors: &[[u8; 3]; LED_COUNT]) {
        self.send_reset();
        for color in colors {
            self.write_byte(color[1]);
            self.write_byte(color[0]);
            self.write_byte(color[2]);
        }
        self.send_reset();
    }
}

struct DfPlayer<TX>
where
    TX: SerialWrite<u8>,
{
    tx: TX,
}

impl<TX> DfPlayer<TX>
where
    TX: SerialWrite<u8>,
{
    fn new(tx: TX) -> Self {
        DfPlayer { tx }
    }

    fn send_command(&mut self, command: u8, param: u16) {
        let mut packet = [0u8; 10];
        packet[0] = 0x7E;
        packet[1] = 0xFF;
        packet[2] = 0x06;
        packet[3] = command;
        packet[4] = 0x00;
        packet[5] = (param >> 8) as u8;
        packet[6] = param as u8;

        let checksum = checksum(&packet[1..7]);
        packet[7] = (checksum >> 8) as u8;
        packet[8] = checksum as u8;
        packet[9] = 0xEF;

        for &byte in packet.iter() {
            self.write_byte_timeout(byte);
        }
    }

    fn write_byte_timeout(&mut self, byte: u8) {
        let mut attempts = 0u16;
        while attempts < DFPLAYER_WRITE_TIMEOUT {
            match self.tx.write(byte) {
                Ok(()) => return,
                Err(nb::Error::WouldBlock) => {
                    attempts = attempts.wrapping_add(1);
                    cortex_m::asm::delay(80);
                }
                Err(nb::Error::Other(_)) => return,
            }
        }
    }

    fn play_track(&mut self, track: u8) {
        self.send_command(0x03, track as u16);
    }

    fn play_configured_track(&mut self, track: u8) {
        if DFPLAYER_USE_MP3_FOLDER {
            self.play_mp3_folder_track(track);
        } else {
            self.play_track(track);
        }
    }

    fn play_mp3_folder_track(&mut self, track: u8) {
        self.send_command(0x12, track as u16);
    }

    fn select_tf_card(&mut self) {
        self.send_command(0x09, 2);
    }

    fn set_volume(&mut self, volume: u8) {
        self.send_command(0x06, volume.min(30) as u16);
    }

    fn reset_module(&mut self) {
        self.send_command(0x0c, 0);
    }

    fn stop(&mut self) {
        self.send_command(0x16, 0);
    }
}

fn checksum(data: &[u8]) -> u16 {
    let sum = data
        .iter()
        .fold(0u16, |acc, byte| acc.wrapping_add(*byte as u16));
    (!sum).wrapping_add(1)
}

struct LampSystem<TX>
where
    TX: SerialWrite<u8>,
{
    a0: ModePin,
    a1: NoisePin,
    ws_a: Ws2815,
    ws_b: Ws2815,
    df_player: DfPlayer<TX>,
    current_mode: LampMode,
    progress: Progress,
    last_animation_tick: u32,
    heartbeat_counter: u8,
    heartbeat_high: bool,
    rng: u32,
    mode_steps: u16,
    sound_steps: u16,
    last_failed_track: u8,
    failed_track_duration_steps: u16,
    played_run_loop: bool,
    audio_boot_steps: u16,
    pending_track: u8,
    pending_loop: bool,
    pending_retry_steps: u16,
    audio_started: bool,
    audio_configured: bool,
    audio_stopped_after_boot: bool,
    frame_a: [[u8; 3]; LED_COUNT],
    frame_b: [[u8; 3]; LED_COUNT],
}

impl<TX> LampSystem<TX>
where
    TX: SerialWrite<u8>,
{
    fn new(a0: ModePin, a1: NoisePin, ws_a: Ws2815, ws_b: Ws2815, df_player: DfPlayer<TX>) -> Self {
        Self {
            a0,
            a1,
            ws_a,
            ws_b,
            df_player,
            current_mode: LampMode::Off,
            progress: Progress::new(),
            last_animation_tick: SYST::get_current(),
            heartbeat_counter: 0,
            heartbeat_high: false,
            rng: 0x1234_abcd,
            mode_steps: 0,
            sound_steps: 0,
            last_failed_track: 0,
            failed_track_duration_steps: failed_track_duration_steps(FAILED_START_MIN_TRACK),
            played_run_loop: false,
            audio_boot_steps: 0,
            pending_track: 0,
            pending_loop: false,
            pending_retry_steps: 0,
            audio_started: false,
            audio_configured: false,
            audio_stopped_after_boot: false,
            frame_a: [[0; 3]; LED_COUNT],
            frame_b: [[0; 3]; LED_COUNT],
        }
    }

    fn read_mode(&self) -> LampMode {
        let a0_low = self.a0.is_low().unwrap_or(false);
        let a1_low = self.a1.is_low().unwrap_or(false);

        match (a0_low, a1_low) {
            (false, false) => LampMode::Off,
            (true, false) => LampMode::Starting,
            (_, true) => LampMode::On,
        }
    }

    fn step(&mut self) {
        self.tick_heartbeat();

        let now = SYST::get_current();
        let elapsed = self.last_animation_tick.wrapping_sub(now) & 0x00ff_ffff;
        self.last_animation_tick = now;
        let mode = self.read_mode();

        if mode != self.current_mode {
            self.mode_steps = 0;
            self.sound_steps = 0;
            self.played_run_loop = false;
            self.pending_track = 0;
            self.pending_loop = false;
            self.pending_retry_steps = 0;
            self.audio_started = false;
            self.progress = Progress::new();

            match mode {
                LampMode::Off => {
                    self.df_player.stop();
                    self.clear();
                    set_relay(false);
                }
                LampMode::Starting => {
                    set_relay(false);
                    self.play_failed_start_track_now();
                }
                LampMode::On => {
                    set_relay(false);
                    self.df_player.set_volume(DFPLAYER_VOLUME);
                    self.df_player.play_configured_track(START_TRACK);
                }
            }
            self.current_mode = mode;
        } else if mode != LampMode::Off {
            self.progress.advance(elapsed);
        }

        match self.current_mode {
            LampMode::Off => self.clear(),
            LampMode::Starting => self.progress.render(&mut self.frame_a, false),
            LampMode::On => {
                set_relay(self.mode_steps >= SUCCESS_START_STEPS);
                self.progress.render(&mut self.frame_a, true);
            }
        }

        self.frame_b = self.frame_a;
        self.update_sound();
        self.update_audio_queue();
        self.ws_a.write_frame(&self.frame_a);
        self.ws_b.write_frame(&self.frame_b);
        cortex_m::asm::delay(UPDATE_DELAY);
    }

    fn clear(&mut self) {
        self.frame_a = [[0; 3]; LED_COUNT];
        self.frame_b = [[0; 3]; LED_COUNT];
    }

    fn update_sound(&mut self) {
        self.mode_steps = self.mode_steps.wrapping_add(1);
        self.sound_steps = self.sound_steps.wrapping_add(1);

        match self.current_mode {
            LampMode::Off => {}
            LampMode::Starting => {
                if self.sound_steps >= self.failed_track_duration_steps {
                    self.sound_steps = 0;
                    self.play_failed_start_track_now();
                }
            }
            LampMode::On => {
                if !self.played_run_loop && self.mode_steps >= SUCCESS_START_STEPS {
                    self.played_run_loop = true;
                    self.sound_steps = 0;
                    self.df_player.set_volume(DFPLAYER_VOLUME);
                    self.df_player.play_configured_track(RUN_TRACK);
                } else if self.played_run_loop && self.sound_steps >= RUN_TRACK_REPEAT_STEPS {
                    self.sound_steps = 0;
                    self.df_player.play_configured_track(RUN_TRACK);
                }
            }
        }
    }

    fn play_failed_start_track_now(&mut self) {
        let track = self.next_failed_track();
        self.failed_track_duration_steps =
            failed_track_duration_steps(track).saturating_add(self.next_u8() as u16);
        self.df_player.set_volume(DFPLAYER_VOLUME);
        self.df_player.play_configured_track(track);
    }

    fn update_audio_queue(&mut self) {
        if self.audio_boot_steps < DFPLAYER_BOOT_DELAY_STEPS {
            self.audio_boot_steps = self.audio_boot_steps.wrapping_add(1);
            return;
        }

        if !self.audio_configured {
            self.df_player.set_volume(DFPLAYER_VOLUME);
            self.audio_configured = true;
            return;
        }

        if self.current_mode == LampMode::Off && !self.audio_stopped_after_boot {
            self.df_player.stop();
            self.audio_stopped_after_boot = true;
            return;
        }

        if self.pending_track == 0 {
            return;
        }

        if self.pending_retry_steps > 0 {
            self.pending_retry_steps -= 1;
            return;
        }

        if self.pending_loop {
            self.df_player.play_configured_track(self.pending_track);
        } else if DFPLAYER_USE_MP3_FOLDER {
            self.df_player.play_mp3_folder_track(self.pending_track);
        } else {
            self.df_player.play_track(self.pending_track);
        }

        if self.audio_started {
            self.pending_track = 0;
            return;
        }

        self.audio_started = true;
        self.pending_retry_steps = DFPLAYER_START_RETRY_STEPS;
    }

    fn next_failed_track(&mut self) -> u8 {
        let mut track = FAILED_START_MIN_TRACK + (self.next_u8() % FAILED_START_TRACKS);
        if track == self.last_failed_track {
            track = FAILED_START_MIN_TRACK
                + ((track - FAILED_START_MIN_TRACK + 1) % FAILED_START_TRACKS);
        }
        self.last_failed_track = track;
        track
    }

    fn tick_heartbeat(&mut self) {
        self.heartbeat_counter = self.heartbeat_counter.wrapping_add(1);
        if self.heartbeat_counter < HEARTBEAT_TOGGLE_FRAMES {
            return;
        }

        self.heartbeat_counter = 0;
        self.heartbeat_high = !self.heartbeat_high;
        gpioa_write_pin(HEARTBEAT_PIN, self.heartbeat_high);
    }

    fn next_u8(&mut self) -> u8 {
        self.rng = next_rng(self.rng);
        (self.rng >> 24) as u8
    }
}

fn next_rng(value: u32) -> u32 {
    value.wrapping_mul(1_664_525).wrapping_add(1_013_904_223)
}

fn failed_track_duration_steps(track: u8) -> u16 {
    let track_offset = (track.saturating_sub(FAILED_START_MIN_TRACK) as u16) * 70;
    FAILED_TRACK_MIN_STEPS + track_offset + (track as u16 % 2) * FAILED_TRACK_RANDOM_STEPS / 2
}

fn gpioa_write_pin(pin_number: u8, high: bool) {
    let bit = 1u32 << pin_number;
    let mask = if high { bit } else { bit << 16 };

    unsafe {
        ptr::write_volatile(GPIOA_BSRR, mask);
    }
}

fn set_relay(on: bool) {
    gpioa_write_pin(RELAY_PIN, on);
}

#[entry]
fn main() -> ! {
    let mut dp = pac::Peripherals::take().unwrap();
    let mut rcc = dp
        .RCC
        .configure()
        .hse(8.mhz(), HSEBypassMode::NotBypassed)
        .sysclk(48.mhz())
        .freeze(&mut dp.FLASH);

    let gpioa = dp.GPIOA.split(&mut rcc);

    let (a0, a1, _heartbeat_pin, _ws_a_pin, _ws_b_pin, _relay_pin, tx_pin, rx_pin) = free(|cs| {
        (
            gpioa.pa0.into_pull_up_input(cs),
            gpioa.pa1.into_pull_up_input(cs),
            gpioa.pa4.into_push_pull_output_hs(cs),
            gpioa.pa6.into_push_pull_output_hs(cs),
            gpioa.pa5.into_push_pull_output_hs(cs),
            gpioa.pa7.into_push_pull_output_hs(cs),
            gpioa.pa9.into_alternate_af1(cs),
            gpioa.pa10.into_alternate_af1(cs),
        )
    });
    gpioa_write_pin(HEARTBEAT_PIN, false);
    set_relay(false);

    let serial: DfSerial = Serial::usart1(dp.USART1, (tx_pin, rx_pin), 9_600.bps(), &mut rcc);

    let mut df_player = DfPlayer::new(serial);
    cortex_m::asm::delay(96_000_000);
    df_player.reset_module();
    cortex_m::asm::delay(96_000_000);
    df_player.select_tf_card();
    cortex_m::asm::delay(8_000_000);
    df_player.set_volume(DFPLAYER_VOLUME);
    cortex_m::asm::delay(8_000_000);
    df_player.stop();

    let mut watchdog = Watchdog::new(dp.IWDG);
    watchdog.start(Hertz(1));

    // SysTick runs freely at HCLK / 8 (6 MHz), wrapping every ~2.8 seconds.
    let mut cp = cortex_m::Peripherals::take().unwrap();
    cp.SYST.set_clock_source(SystClkSource::External);
    cp.SYST.set_reload(0x00ff_ffff);
    cp.SYST.clear_current();
    cp.SYST.enable_counter();
    cortex_m::asm::delay(16);

    let mut lamp = LampSystem::new(a0, a1, Ws2815::new(6), Ws2815::new(5), df_player);

    loop {
        lamp.step();
        watchdog.feed();
    }
}
