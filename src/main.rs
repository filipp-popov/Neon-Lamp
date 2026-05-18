#![no_std]
#![no_main]

use core::ptr;
use cortex_m::interrupt::free;
use cortex_m_rt::entry;
use embedded_hal::digital::v2::InputPin;
use embedded_hal::serial::Write as SerialWrite;
use panic_halt as _;
use stm32f0xx_hal::gpio::{
    gpioa::PA0, gpioa::PA1, gpioa::PA10, gpioa::PA9, Alternate, Input, PullUp, AF1,
};
use stm32f0xx_hal::pac;
use stm32f0xx_hal::prelude::*;
use stm32f0xx_hal::rcc::HSEBypassMode;
use stm32f0xx_hal::serial::Serial;
use stm32f0xx_hal::time::Hertz;
use stm32f0xx_hal::watchdog::Watchdog;

const LED_COUNT: usize = 64;
const UPDATE_DELAY: u32 = 100_000;
const WS_RESET_DELAY: u32 = 500_000;
const WS_T0L: u32 = 10;
const WS_T1H: u32 = 10;
const WS_T1L: u32 = 0;
const DIAGNOSTIC_STATIC_TEST: bool = false;
const DIAGNOSTIC_GPIO_TEST: bool = false;
const DIAGNOSTIC_OFF_FRAME_TEST: bool = false;
const DIAGNOSTIC_SCOPE_PATTERN_TEST: bool = false;
const DIAGNOSTIC_DFPLAYER_TEST: bool = false;
const DIAGNOSTIC_UART_PATTERN_TEST: bool = false;
const DIAGNOSTIC_INVERT_WS_SIGNAL: bool = false;
const HEARTBEAT_PIN: u8 = 4;
const HEARTBEAT_TOGGLE_FRAMES: u8 = 64;
const SCOPE_PATTERN_BYTES: usize = 64;
const GPIOA_BSRR: *mut u32 = 0x4800_0018 as *mut u32;
const RELAY_PIN: u8 = 7;
const TEST_BRIGHTNESS: u8 = 64;
const DFPLAYER_TRACK_SECONDS: u8 = 20;
const DFPLAYER_USE_MP3_FOLDER: bool = true;
const DFPLAYER_ENABLED: bool = true;
const START_TRACK: u8 = 1;
const FAILED_START_MIN_TRACK: u8 = 2;
const FAILED_START_TRACKS: u8 = 3;
const RUN_TRACK: u8 = 5;
const SUCCESS_START_STEPS: u16 = 85;
const RUN_TRACK_REPEAT_STEPS: u16 = 500;
const FAILED_TRACK_MIN_STEPS: u16 = 360;
const FAILED_TRACK_RANDOM_STEPS: u16 = 320;
const STARTING_END_GLOW_PIXELS: usize = 6;
const STARTING_RED_BASE_PIXELS: usize = 3;
const STARTING_DISCHARGE_CYCLE_STEPS: u16 = 30;
const STARTING_FULL_FLASH_STEPS: u16 = 170;
const STARTING_FULL_FLASH_DURATION_STEPS: u16 = 7;
const STARTING_CENTER_DIM_PIXELS: usize = LED_COUNT * 30 / 100;
const STARTING_DISCHARGE_DECAY_STEPS: u8 = 3;
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
    inverted: bool,
}

impl Ws2815 {
    fn new(pin_number: u8, inverted: bool) -> Self {
        let bit = 1u32 << pin_number;
        Self {
            set_mask: bit,
            clear_mask: bit << 16,
            inverted,
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
        if self.inverted {
            self.write_mask(self.clear_mask);
        } else {
            self.write_mask(self.set_mask);
        }
    }

    #[inline(always)]
    fn set_low(&self) {
        if self.inverted {
            self.write_mask(self.set_mask);
        } else {
            self.write_mask(self.clear_mask);
        }
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
            self.write_byte(color[0]);
            self.write_byte(color[1]);
            self.write_byte(color[2]);
        }
        self.send_reset();
    }

    fn write_repeated_byte(&mut self, byte: u8, count: usize) {
        for _ in 0..count {
            self.write_byte(byte);
        }
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

    fn init(&mut self) {}

    fn send_command(&mut self, command: u8, param: u16) {
        if !DFPLAYER_ENABLED {
            return;
        }

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

    fn write_uart_pattern(&mut self) {
        self.write_byte_timeout(0x55);
        self.write_byte_timeout(0x00);
        self.write_byte_timeout(0xff);
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

    fn loop_track(&mut self, track: u8) {
        self.send_command(0x08, track as u16);
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
    let sum = data.iter().fold(0u16, |acc, byte| acc.wrapping_add(*byte as u16));
    (!sum).wrapping_add(1)
}

struct LampSystem<TX>
where
    TX: SerialWrite<u8>,
{
    a0: ModePin,
    a1: NoisePin,
    ws_a: Ws2815,
    df_player: DfPlayer<TX>,
    current_mode: LampMode,
    animation_index: usize,
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
    discharge_steps: u8,
    discharge_left_reach: u8,
    discharge_right_reach: u8,
    discharge_side_mode: u8,
    discharge_intensity: u8,
    frame_a: [[u8; 3]; LED_COUNT],
}

impl<TX> LampSystem<TX>
where
    TX: SerialWrite<u8>,
{
    fn new(
        a0: ModePin,
        a1: NoisePin,
        ws_a: Ws2815,
        df_player: DfPlayer<TX>,
    ) -> Self {
        Self {
            a0,
            a1,
            ws_a,
            df_player,
            current_mode: LampMode::Off,
            animation_index: 0,
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
            discharge_steps: 0,
            discharge_left_reach: 0,
            discharge_right_reach: 0,
            discharge_side_mode: 0,
            discharge_intensity: 0,
            frame_a: [[0; 3]; LED_COUNT],
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

        if DIAGNOSTIC_SCOPE_PATTERN_TEST {
            self.ws_a.write_repeated_byte(0x00, SCOPE_PATTERN_BYTES);
            cortex_m::asm::delay(UPDATE_DELAY);
            return;
        }

        if DIAGNOSTIC_OFF_FRAME_TEST {
            self.clear();
            self.ws_a.write_frame(&self.frame_a);
            cortex_m::asm::delay(UPDATE_DELAY);
            return;
        }

        if DIAGNOSTIC_STATIC_TEST {
            self.render_diagnostic_static_test();
            self.ws_a.write_frame(&self.frame_a);
            cortex_m::asm::delay(UPDATE_DELAY);
            return;
        }

        let mode = self.read_mode();

        if mode != self.current_mode {
            self.mode_steps = 0;
            self.sound_steps = 0;
            self.played_run_loop = false;
            self.pending_track = 0;
            self.pending_loop = false;
            self.pending_retry_steps = 0;
            self.audio_started = false;
            self.discharge_steps = 0;

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
        }

        match self.current_mode {
            LampMode::Off => self.clear(),
            LampMode::Starting => self.render_starting(),
            LampMode::On => self.render_on(),
        }

        self.update_sound();
        self.update_audio_queue();
        self.ws_a.write_frame(&self.frame_a);
        cortex_m::asm::delay(UPDATE_DELAY);
    }

    fn clear(&mut self) {
        self.frame_a = [[0; 3]; LED_COUNT];
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

    fn render_diagnostic_static_test(&mut self) {
        for led in self.frame_a.iter_mut() {
            *led = [12, 0, 0];
        }
    }

    fn render_starting(&mut self) {
        self.fade(34);
        self.keep_center_dim();
        self.add_warm_red_ends();

        if (self.mode_steps % STARTING_FULL_FLASH_STEPS) < STARTING_FULL_FLASH_DURATION_STEPS {
            for led in self.frame_a.iter_mut() {
                *led = warm_white(255);
            }
            self.animation_index = self.animation_index.wrapping_add(1);
            return;
        }

        if self.discharge_steps > 0 {
            self.render_active_discharge();
            self.discharge_steps -= 1;
        } else if (self.mode_steps % STARTING_DISCHARGE_CYCLE_STEPS) < 18
            && (self.next_u8() & 0x1f) == 0
        {
            self.start_discharge();
            self.render_active_discharge();
            self.discharge_steps -= 1;
        }

        if (self.next_u8() & 0x3f) == 0 {
            self.add_warm_red_ends();
        }

        self.animation_index = self.animation_index.wrapping_add(1);
    }

    fn render_on(&mut self) {
        if self.mode_steps < SUCCESS_START_STEPS {
            set_relay(false);
            self.render_success_start();
            self.animation_index = self.animation_index.wrapping_add(1);
            return;
        }

        set_relay(true);
        self.clear();

        self.animation_index = self.animation_index.wrapping_add(1);
    }

    fn render_success_start(&mut self) {
        self.fade(5);

        let ramp = (self.mode_steps as u32 * 88 / SUCCESS_START_STEPS as u32) as u8;
        let global_flicker = match self.next_u8() & 0x0f {
            0 => 42,
            1..=3 => 22,
            4..=8 => 10,
            _ => 0,
        };
        let base = ramp.saturating_add(global_flicker);

        let mut rng = self.rng;
        for led in self.frame_a.iter_mut() {
            rng = next_rng(rng);
            let local_noise = ((rng >> 24) as u8 & 0x03).saturating_sub(1);
            let value = base.saturating_add(local_noise);
            *led = warm_white(value);
        }
        self.rng = rng;

        if (self.next_u8() & 0x0f) == 0 {
            let pos = self.next_index();
            self.add_glow(pos, warm_white(150), 5);
        }
    }

    fn fade(&mut self, amount: u8) {
        for led in self.frame_a.iter_mut() {
            led[0] = led[0].saturating_sub(amount);
            led[1] = led[1].saturating_sub(amount);
            led[2] = led[2].saturating_sub(amount);
        }
    }

    fn add_warm_red_ends(&mut self) {
        let extra = (self.next_u8() & 0x01) as usize;
        let end_len = (STARTING_RED_BASE_PIXELS + extra).min(LED_COUNT / 2 - 2);
        let pulse = self.next_u8() & 0x1f;

        for pos in 0..end_len {
            let falloff = pos as u8 * 12;
            let value = 124u8.saturating_add(pulse).saturating_sub(falloff);
            let color = warm_red(value, pos);
            self.add_to_led(pos, color);
            self.add_to_led(LED_COUNT - 1 - pos, color);
        }
    }

    fn keep_center_dim(&mut self) {
        let start = (LED_COUNT - STARTING_CENTER_DIM_PIXELS) / 2;
        let end = start + STARTING_CENTER_DIM_PIXELS;

        for pos in start..end {
            self.frame_a[pos][0] = self.frame_a[pos][0].min(18);
            self.frame_a[pos][1] = self.frame_a[pos][1].min(16);
            self.frame_a[pos][2] = self.frame_a[pos][2].min(14);
        }
    }

    fn start_discharge(&mut self) {
        let half_dead = STARTING_CENTER_DIM_PIXELS / 2;
        let left_limit = (LED_COUNT / 2).saturating_sub(half_dead + 1);
        let max_reach = left_limit.saturating_sub(STARTING_END_GLOW_PIXELS);
        let min_reach = max_reach / 3;

        let reach_span = max_reach.saturating_sub(min_reach).saturating_add(1);
        self.discharge_left_reach = (min_reach + (self.next_u8() as usize % reach_span)) as u8;
        self.discharge_right_reach = (min_reach + (self.next_u8() as usize % reach_span)) as u8;
        self.discharge_side_mode = self.next_u8() & 0x03;
        self.discharge_intensity = 215 + (self.next_u8() & 0x2f);
        self.discharge_steps = STARTING_DISCHARGE_DECAY_STEPS;
    }

    fn render_active_discharge(&mut self) {
        let scale = self.discharge_steps.max(1) as usize;
        let left_reach =
            (self.discharge_left_reach as usize * scale / STARTING_DISCHARGE_DECAY_STEPS as usize)
                .max(1);
        let right_reach =
            (self.discharge_right_reach as usize * scale / STARTING_DISCHARGE_DECAY_STEPS as usize)
                .max(1);

        if self.discharge_side_mode != 1 {
            self.add_discharge_from_left(left_reach, self.discharge_intensity);
        }
        if self.discharge_side_mode != 2 {
            let dim = if self.discharge_side_mode == 3 { 42 } else { 10 };
            self.add_discharge_from_right(right_reach, self.discharge_intensity.saturating_sub(dim));
        }
    }

    fn add_discharge_from_left(&mut self, reach: usize, intensity: u8) {
        let start = STARTING_RED_BASE_PIXELS.saturating_sub(1);
        let end = (start + reach).min((LED_COUNT - STARTING_CENTER_DIM_PIXELS) / 2 - 1);
        self.add_attached_discharge(start, end, intensity, true);
    }

    fn add_discharge_from_right(&mut self, reach: usize, intensity: u8) {
        let end = LED_COUNT - STARTING_RED_BASE_PIXELS;
        let start = end.saturating_sub(reach).max((LED_COUNT + STARTING_CENTER_DIM_PIXELS) / 2);
        self.add_attached_discharge(start, end, intensity, false);
    }

    fn add_attached_discharge(&mut self, start: usize, end: usize, intensity: u8, from_left: bool) {
        if start > end {
            return;
        }

        let len = end - start + 1;

        for (i, pos) in (start..=end).enumerate() {
            let distance_from_tip = if from_left {
                len.saturating_sub(1).saturating_sub(i)
            } else {
                i
            };
            let inner_decay = (distance_from_tip as u8).saturating_mul(10);
            let jitter = self.next_u8() & 0x2f;
            let value = intensity.saturating_sub(inner_decay).saturating_sub(jitter);
            self.add_to_led(pos, warm_white(value));
        }
    }

    fn add_glow(&mut self, center: usize, color: [u8; 3], radius: usize) {
        for offset in 0..=radius {
            let divisor = (offset + 1) as u8;
            let scaled = [color[0] / divisor, color[1] / divisor, color[2] / divisor];

            self.add_to_led(center.wrapping_add(offset) % LED_COUNT, scaled);
            if offset > 0 {
                self.add_to_led((center + LED_COUNT - offset) % LED_COUNT, scaled);
            }
        }
    }

    fn add_to_led(&mut self, pos: usize, color: [u8; 3]) {
        self.frame_a[pos][0] = self.frame_a[pos][0].saturating_add(color[0]);
        self.frame_a[pos][1] = self.frame_a[pos][1].saturating_add(color[1]);
        self.frame_a[pos][2] = self.frame_a[pos][2].saturating_add(color[2]);
    }

    fn next_index(&mut self) -> usize {
        (self.next_u8() as usize) % LED_COUNT
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

fn warm_white(value: u8) -> [u8; 3] {
    [
        value.saturating_sub(value / 16),
        value.saturating_sub(value / 14),
        value.saturating_sub(value / 10),
    ]
}

fn warm_red(value: u8, pos: usize) -> [u8; 3] {
    if pos < 3 {
        [value, value / 7, value / 20]
    } else {
        [value, value / 4, value / 14]
    }
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

    let (a0, a1, _heartbeat_pin, _ws_a_pin, _ws_b_pin, tx_pin, rx_pin) = free(|cs| {
        (
            gpioa.pa0.into_pull_up_input(cs),
            gpioa.pa1.into_pull_up_input(cs),
            gpioa.pa4.into_push_pull_output_hs(cs),
            gpioa.pa6.into_push_pull_output_hs(cs),
            gpioa.pa7.into_push_pull_output_hs(cs),
            gpioa.pa9.into_alternate_af1(cs),
            gpioa.pa10.into_alternate_af1(cs),
        )
    });
    gpioa_write_pin(HEARTBEAT_PIN, false);
    set_relay(false);

    if DIAGNOSTIC_GPIO_TEST {
        let mut high = false;
        gpioa_write_pin(6, false);
        gpioa_write_pin(7, false);

        loop {
            high = !high;
            gpioa_write_pin(4, high);
            cortex_m::asm::delay(8_000_000);
        }
    }

    if DIAGNOSTIC_OFF_FRAME_TEST {
        let mut ws_a = Ws2815::new(6, DIAGNOSTIC_INVERT_WS_SIGNAL);
        let frame = [[0u8; 3]; LED_COUNT];
        let mut heartbeat = false;

        gpioa_write_pin(6, false);
        gpioa_write_pin(7, false);
        cortex_m::asm::delay(8_000_000);

        loop {
            heartbeat = !heartbeat;
            gpioa_write_pin(HEARTBEAT_PIN, heartbeat);
            ws_a.write_frame(&frame);
            gpioa_write_pin(6, false);
            gpioa_write_pin(7, false);
            cortex_m::asm::delay(8_000_000);
        }
    }

    if DIAGNOSTIC_SCOPE_PATTERN_TEST {
        let mut ws_a = Ws2815::new(6, DIAGNOSTIC_INVERT_WS_SIGNAL);
        let mut heartbeat = false;

        loop {
            heartbeat = !heartbeat;
            gpioa_write_pin(HEARTBEAT_PIN, heartbeat);
            ws_a.write_repeated_byte(0x00, SCOPE_PATTERN_BYTES);
            cortex_m::asm::delay(2_000_000);
        }
    }

    if DIAGNOSTIC_STATIC_TEST {
        let mut ws_a = Ws2815::new(6, DIAGNOSTIC_INVERT_WS_SIGNAL);
        let frame_a = [[TEST_BRIGHTNESS, 0, 0]; LED_COUNT];
        let mut heartbeat = false;

        loop {
            heartbeat = !heartbeat;
            gpioa_write_pin(HEARTBEAT_PIN, heartbeat);
            ws_a.write_frame(&frame_a);
            cortex_m::asm::delay(2_000_000);
        }
    }

    let serial: DfSerial = Serial::usart1(dp.USART1, (tx_pin, rx_pin), 9_600.bps(), &mut rcc);

    if DIAGNOSTIC_DFPLAYER_TEST {
        let mut ws_a = Ws2815::new(6, DIAGNOSTIC_INVERT_WS_SIGNAL);
        let blank = [[0u8; 3]; LED_COUNT];
        let mut df_player = DfPlayer::new(serial);
        let mut watchdog = Watchdog::new(dp.IWDG);
        let mut heartbeat = false;

        for _ in 0..4 {
            heartbeat = !heartbeat;
            gpioa_write_pin(HEARTBEAT_PIN, heartbeat);
            ws_a.write_frame(&blank);
            cortex_m::asm::delay(4_000_000);
        }

        if DIAGNOSTIC_UART_PATTERN_TEST {
            watchdog.start(Hertz(1));

            loop {
                heartbeat = !heartbeat;
                gpioa_write_pin(HEARTBEAT_PIN, heartbeat);
                ws_a.write_frame(&blank);

                for _ in 0..64 {
                    df_player.write_uart_pattern();
                }

                watchdog.feed();
                cortex_m::asm::delay(500_000);
            }
        }

        df_player.reset_module();
        cortex_m::asm::delay(96_000_000);
        df_player.select_tf_card();
        cortex_m::asm::delay(8_000_000);
        df_player.set_volume(30);
        cortex_m::asm::delay(8_000_000);
        watchdog.start(Hertz(1));

        loop {
            heartbeat = !heartbeat;
            gpioa_write_pin(HEARTBEAT_PIN, heartbeat);

            df_player.set_volume(30);
            df_player.play_track(1);

            for _ in 0..80 {
                heartbeat = !heartbeat;
                gpioa_write_pin(HEARTBEAT_PIN, heartbeat);
                ws_a.write_frame(&blank);
                watchdog.feed();
                cortex_m::asm::delay(1_000_000);
            }
        }
    }

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

    let mut lamp = LampSystem::new(
        a0,
        a1,
        Ws2815::new(6, DIAGNOSTIC_INVERT_WS_SIGNAL),
        df_player,
    );

    lamp.df_player.init();

    loop {
        lamp.step();
        watchdog.feed();
    }
}
