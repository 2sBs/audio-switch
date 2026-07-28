#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DriftControllerConfig {
    pub target_frames: usize,
    pub proportional_gain: f64,
    pub max_adjustment_ppm: f64,
}

impl DriftControllerConfig {
    pub fn for_target(target_frames: usize) -> Self {
        Self {
            target_frames,
            proportional_gain: 0.05,
            max_adjustment_ppm: 1_000.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DriftController {
    config: DriftControllerConfig,
    smoothed_error: f64,
}

impl DriftController {
    pub fn new(config: DriftControllerConfig) -> Self {
        assert!(config.target_frames > 0);
        assert!(config.proportional_gain > 0.0);
        assert!(config.max_adjustment_ppm > 0.0);
        Self {
            config,
            smoothed_error: 0.0,
        }
    }

    /// Returns the ratio applied to source consumption. A ratio above 1 consumes
    /// source frames faster when the queue is too full; below 1 preserves frames
    /// when the queue is too empty.
    pub fn ratio(&mut self, buffered_frames: usize) -> f64 {
        let raw_error = (buffered_frames as f64 - self.config.target_frames as f64)
            / self.config.target_frames as f64;
        self.smoothed_error = self.smoothed_error * 0.95 + raw_error * 0.05;

        let max_adjustment = self.config.max_adjustment_ppm / 1_000_000.0;
        let adjustment = (self.smoothed_error * self.config.proportional_gain)
            .clamp(-max_adjustment, max_adjustment);
        1.0 + adjustment
    }

    pub fn reset(&mut self) {
        self.smoothed_error = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speeds_up_when_buffer_is_too_full() {
        let mut controller = DriftController::new(DriftControllerConfig::for_target(480));
        let ratio = (0..100).fold(1.0, |_, _| controller.ratio(960));
        assert!(ratio > 1.0);
        assert!(ratio <= 1.001);
    }

    #[test]
    fn slows_down_when_buffer_is_too_empty() {
        let mut controller = DriftController::new(DriftControllerConfig::for_target(480));
        let ratio = (0..100).fold(1.0, |_, _| controller.ratio(0));
        assert!(ratio < 1.0);
        assert!(ratio >= 0.999);
    }
}
