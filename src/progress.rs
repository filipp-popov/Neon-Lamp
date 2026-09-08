pub const COLUMN_COUNT: usize = 7;
pub const COLUMN_HEIGHT: usize = 14;
pub const LED_COUNT: usize = COLUMN_COUNT * COLUMN_HEIGHT;
const PULSE_TICKS: u32 = 6_000_000; // One second at HCLK / 8.
const GREEN: [u8; 3] = [0, 255, 0];
const BLUE: [u8; 3] = [0, 0, 255];

pub struct Progress {
    phase: u32,
    filled: usize,
}

impl Progress {
    pub fn new() -> Self {
        Self {
            phase: 0,
            filled: 0,
        }
    }

    pub fn advance(&mut self, elapsed_ticks: u32) {
        self.phase += elapsed_ticks;
        while self.phase >= PULSE_TICKS {
            self.phase -= PULSE_TICKS;
            // Keep the full bar glowing while the runner continues cycling.
            self.filled = (self.filled + 1).min(COLUMN_HEIGHT);
        }
    }

    pub fn render(&self, frame: &mut [[u8; 3]; LED_COUNT], blue_fill: bool) {
        let (fill, runner) = if blue_fill {
            (BLUE, GREEN)
        } else {
            (GREEN, BLUE)
        };
        let row = (self.phase * COLUMN_HEIGHT as u32 / PULSE_TICKS) as usize;
        for column in frame.chunks_exact_mut(COLUMN_HEIGHT) {
            for (height, led) in column.iter_mut().enumerate() {
                // The runner temporarily overrides filled pixels, preserving its color.
                *led = if height == row {
                    runner
                } else if height < self.filled {
                    fill
                } else {
                    [0; 3]
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_traverses_every_column_and_adds_one_pixel_per_second() {
        let mut progress = Progress::new();
        let mut frame = [[0; 3]; LED_COUNT];
        for row in 0..COLUMN_HEIGHT {
            progress.phase =
                (row as u32 * PULSE_TICKS + COLUMN_HEIGHT as u32 - 1) / COLUMN_HEIGHT as u32;
            progress.render(&mut frame, false);
            for column in frame.chunks_exact(COLUMN_HEIGHT) {
                assert_eq!(column[row], BLUE);
                assert_eq!(column.iter().filter(|&&c| c != [0; 3]).count(), 1);
            }
        }
        progress.advance(PULSE_TICKS - progress.phase);
        assert_eq!(progress.filled, 1);
        progress.advance(PULSE_TICKS / 2);
        progress.render(&mut frame, false);
        for column in frame.chunks_exact(COLUMN_HEIGHT) {
            assert_eq!(column[0], GREEN);
            assert_eq!(column[7], BLUE);
            assert_eq!(column.iter().filter(|&&c| c != [0; 3]).count(), 2);
        }
        progress.render(&mut frame, true);
        assert_eq!(frame[0], BLUE);
        assert_eq!(frame[7], GREEN);
    }

    #[test]
    fn full_bar_stays_lit_with_runner_and_new_mode_starts_empty() {
        let mut progress = Progress::new();
        for filled in 1..=COLUMN_HEIGHT {
            progress.advance(PULSE_TICKS);
            assert_eq!(progress.filled, filled);
        }
        let mut frame = [[0; 3]; LED_COUNT];
        progress.render(&mut frame, false);
        assert!(frame.iter().all(|&c| c == GREEN || c == BLUE));
        for _ in 0..20 {
            progress.advance(PULSE_TICKS);
            assert_eq!(progress.filled, COLUMN_HEIGHT);
        }
        progress.advance(PULSE_TICKS / 2);
        for blue_fill in [false, true] {
            progress.render(&mut frame, blue_fill);
            let (fill, runner) = if blue_fill {
                (BLUE, GREEN)
            } else {
                (GREEN, BLUE)
            };
            for column in frame.chunks_exact(COLUMN_HEIGHT) {
                assert_eq!(column[7], runner);
                assert_eq!(
                    column.iter().filter(|&&c| c == fill).count(),
                    COLUMN_HEIGHT - 1
                );
            }
        }
        let fresh = Progress::new();
        assert_eq!((fresh.phase, fresh.filled), (0, 0));
    }
}
