use std::f32::consts::TAU;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CueKind {
    Primary,
    Secondary,
}

pub fn synthesize_cue(kind: CueKind, sample_rate: u32, level_db: f32) -> Vec<f32> {
    let notes = match kind {
        CueKind::Primary => [660.0, 440.0],
        CueKind::Secondary => [440.0, 660.0],
    };
    let note_frames = (sample_rate as f32 * 0.07) as usize;
    let gap_frames = (sample_rate as f32 * 0.03) as usize;
    let fade_frames = (sample_rate as f32 * 0.005) as usize;
    let amplitude = 10.0f32.powf(level_db / 20.0);
    let mut samples = Vec::with_capacity(note_frames * 2 + gap_frames);

    for (note_index, frequency) in notes.into_iter().enumerate() {
        for frame in 0..note_frames {
            let envelope = if fade_frames == 0 {
                1.0
            } else if frame < fade_frames {
                frame as f32 / fade_frames as f32
            } else if frame + fade_frames >= note_frames {
                (note_frames - frame - 1) as f32 / fade_frames as f32
            } else {
                1.0
            }
            .max(0.0);
            let phase = TAU * frequency * frame as f32 / sample_rate as f32;
            samples.push(phase.sin() * amplitude * envelope);
        }
        if note_index == 0 {
            samples.resize(samples.len() + gap_frames, 0.0);
        }
    }
    samples
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cue_has_expected_duration_and_safe_peak() {
        let samples = synthesize_cue(CueKind::Secondary, 48_000, -18.0);
        assert_eq!(samples.len(), 8_160);
        assert!(samples.iter().all(|sample| sample.abs() <= 0.126));
        assert_eq!(samples[0], 0.0);
        assert_eq!(samples.last().copied(), Some(0.0));
    }

    #[test]
    fn cues_are_directionally_distinct() {
        assert_ne!(
            synthesize_cue(CueKind::Primary, 8_000, -18.0),
            synthesize_cue(CueKind::Secondary, 8_000, -18.0)
        );
    }
}
