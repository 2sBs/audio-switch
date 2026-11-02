//! Platform-neutral pieces of a capture -> render bridge: a lock-free ring
//! between two device clocks, drift-corrected reading, and route counters.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rtrb::{Consumer, Producer, RingBuffer};

use super::{DriftController, DriftControllerConfig};

const RING_CAPACITY_MULTIPLIER: usize = 8;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RouteDiagnostics {
    pub captured_frames: u64,
    pub rendered_frames: u64,
    pub underrun_frames: u64,
    pub overrun_frames: u64,
    pub discontinuities: u64,
}

#[derive(Debug, Default)]
pub(crate) struct AtomicDiagnostics {
    pub captured_frames: AtomicU64,
    pub rendered_frames: AtomicU64,
    pub underrun_frames: AtomicU64,
    pub overrun_frames: AtomicU64,
    pub discontinuities: AtomicU64,
}

impl AtomicDiagnostics {
    pub fn snapshot(&self) -> RouteDiagnostics {
        RouteDiagnostics {
            captured_frames: self.captured_frames.load(Ordering::Relaxed),
            rendered_frames: self.rendered_frames.load(Ordering::Relaxed),
            underrun_frames: self.underrun_frames.load(Ordering::Relaxed),
            overrun_frames: self.overrun_frames.load(Ordering::Relaxed),
            discontinuities: self.discontinuities.load(Ordering::Relaxed),
        }
    }
}

/// Creates an empty cross-clock ring. The reader stays silent until capture has
/// filled it to `target_frames`, so slow-starting input devices do not
/// register as underruns.
pub(crate) fn ring(target_frames: usize, channels: usize) -> (Producer<f32>, Consumer<f32>) {
    let capacity_samples = target_frames
        .saturating_mul(channels)
        .saturating_mul(RING_CAPACITY_MULTIPLIER)
        .max(channels * 64);
    RingBuffer::<f32>::new(capacity_samples)
}

/// Creates the cross-clock ring, pre-filled with `target_frames` of silence so
/// the reader starts immediately at its steady-state fill level.
#[cfg(windows)]
pub(crate) fn primed_ring(target_frames: usize, channels: usize) -> (Producer<f32>, Consumer<f32>) {
    let (mut producer, consumer) = ring(target_frames, channels);
    for _ in 0..target_frames.saturating_mul(channels) {
        let _ = producer.push(0.0);
    }
    (producer, consumer)
}

pub(crate) fn frames_for_ms(sample_rate: u32, milliseconds: u32) -> usize {
    ((sample_rate as u64 * milliseconds as u64) / 1_000).max(1) as usize
}

/// Reads the ring at the render clock's pace. Linear interpolation absorbs both
/// a fixed sample-rate difference (`rate_ratio`) and slow clock drift, which the
/// drift controller tracks through the ring's fill level.
#[derive(Debug)]
pub(crate) struct AdaptiveReader {
    consumer: Consumer<f32>,
    channels: usize,
    rate_ratio: f64,
    current: Vec<f32>,
    next: Vec<f32>,
    phase: f64,
    target_frames: usize,
    initialized: bool,
    drift: DriftController,
    diagnostics: Arc<AtomicDiagnostics>,
}

impl AdaptiveReader {
    /// `rate_ratio` is source frames consumed per rendered frame at nominal
    /// rates: `source_rate / render_rate`.
    pub fn new(
        consumer: Consumer<f32>,
        channels: usize,
        target_frames: usize,
        rate_ratio: f64,
        diagnostics: Arc<AtomicDiagnostics>,
    ) -> Self {
        Self {
            consumer,
            channels,
            rate_ratio,
            current: vec![0.0; channels],
            next: vec![0.0; channels],
            phase: 0.0,
            target_frames,
            initialized: false,
            drift: DriftController::new(DriftControllerConfig::for_target(target_frames)),
            diagnostics,
        }
    }

    /// Fills interleaved `destination` frames. Allocation-free, so it is safe
    /// on a real-time audio thread.
    pub fn render(&mut self, destination: &mut [f32]) {
        debug_assert_eq!(destination.len() % self.channels, 0);
        if !self.initialized {
            if self.consumer.slots() < self.target_frames * self.channels {
                destination.fill(0.0);
                return;
            }
            Self::pop_frame_from(&mut self.consumer, &mut self.current, &self.diagnostics);
            Self::pop_frame_from(&mut self.consumer, &mut self.next, &self.diagnostics);
            self.initialized = true;
        }
        let buffered_frames = self.consumer.slots() / self.channels;
        let step = self.drift.ratio(buffered_frames) * self.rate_ratio;

        for output in destination.chunks_exact_mut(self.channels) {
            for (channel, sample) in output.iter_mut().enumerate() {
                *sample = self.current[channel]
                    + (self.next[channel] - self.current[channel]) * self.phase as f32;
            }
            self.phase += step;
            while self.phase >= 1.0 {
                self.current.copy_from_slice(&self.next);
                Self::pop_frame_from(&mut self.consumer, &mut self.next, &self.diagnostics);
                self.phase -= 1.0;
            }
        }
    }

    fn pop_frame_from(
        consumer: &mut Consumer<f32>,
        destination: &mut [f32],
        diagnostics: &AtomicDiagnostics,
    ) {
        if consumer.slots() < destination.len() {
            destination.fill(0.0);
            diagnostics.underrun_frames.fetch_add(1, Ordering::Relaxed);
            return;
        }
        for sample in destination {
            *sample = consumer
                .pop()
                .expect("the complete frame was available in the ring buffer");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn millisecond_conversion_never_returns_an_empty_buffer() {
        assert_eq!(frames_for_ms(48_000, 30), 1_440);
        assert_eq!(frames_for_ms(48_000, 0), 1);
    }

    #[test]
    fn adaptive_reader_waits_for_the_target_fill_before_reading() {
        let (mut producer, consumer) = RingBuffer::<f32>::new(8);
        let diagnostics = Arc::new(AtomicDiagnostics::default());
        let mut reader = AdaptiveReader::new(consumer, 1, 4, 1.0, diagnostics.clone());
        producer.push(1.0).unwrap();
        let mut output = [1.0_f32; 4];
        reader.render(&mut output);
        assert_eq!(output, [0.0; 4]);
        assert_eq!(reader.consumer.slots(), 1);
        assert_eq!(diagnostics.snapshot().underrun_frames, 0);
    }

    #[test]
    fn adaptive_reader_outputs_silence_on_underflow() {
        let (mut producer, consumer) = RingBuffer::<f32>::new(8);
        let diagnostics = Arc::new(AtomicDiagnostics::default());
        let mut reader = AdaptiveReader::new(consumer, 2, 2, 1.0, diagnostics.clone());
        for _ in 0..4 {
            producer.push(0.5).unwrap();
        }
        let mut output = [1.0_f32; 8];
        reader.render(&mut output);
        assert_eq!(output[6..], [0.0; 2]);
        assert!(diagnostics.snapshot().underrun_frames > 0);
    }

    #[test]
    fn rate_ratio_consumes_source_frames_at_the_source_rate() {
        // 44.1 kHz source into a 48 kHz render clock: 480 rendered frames should
        // consume ~441 source frames.
        let (mut producer, consumer) = RingBuffer::<f32>::new(2_000);
        for _ in 0..1_000 {
            producer.push(0.5).unwrap();
        }
        let mut reader =
            AdaptiveReader::new(consumer, 1, 1_000, 44_100.0 / 48_000.0, Arc::default());
        let mut output = [0.0_f32; 480];
        reader.render(&mut output);
        let consumed = 1_000 - reader.consumer.slots();
        assert!((440..=443).contains(&consumed), "consumed {consumed}");
    }
}
