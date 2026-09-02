use serde::Serialize;
use std::time::Instant;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    #[default]
    Idle,
    Listening,
    Processing,
    Complete,
    Error,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    pub phase: Phase,
    pub message: String,
    pub elapsed_ms: u128,
    pub captured_samples: usize,
    pub sample_rate: u32,
    pub last_text: Option<String>,
    pub asr_ms: u128,
    pub cleanup_ms: u128,
}

impl Default for EngineStatus {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            message: "Hold your shortcut to talk".into(),
            elapsed_ms: 0,
            captured_samples: 0,
            sample_rate: 0,
            last_text: None,
            asr_ms: 0,
            cleanup_ms: 0,
        }
    }
}

pub struct Pipeline {
    pub status: EngineStatus,
    started_at: Option<Instant>,
}

impl Default for Pipeline {
    fn default() -> Self {
        Self {
            status: EngineStatus::default(),
            started_at: None,
        }
    }
}

impl Pipeline {
    pub fn import_processing(&mut self, captured_samples: usize, sample_rate: u32) -> bool {
        if !matches!(
            self.status.phase,
            Phase::Idle | Phase::Complete | Phase::Error
        ) {
            return false;
        }
        self.started_at = Some(Instant::now());
        self.status = EngineStatus {
            phase: Phase::Processing,
            message: "Transcribing imported file locally…".into(),
            captured_samples,
            sample_rate,
            ..EngineStatus::default()
        };
        true
    }

    pub fn begin(&mut self) -> bool {
        if !matches!(
            self.status.phase,
            Phase::Idle | Phase::Complete | Phase::Error
        ) {
            return false;
        }
        self.started_at = Some(Instant::now());
        self.status = EngineStatus {
            phase: Phase::Listening,
            message: "Listening… release to transcribe".into(),
            ..EngineStatus::default()
        };
        true
    }

    pub fn processing(&mut self, captured_samples: usize, sample_rate: u32) -> bool {
        if self.status.phase != Phase::Listening {
            return false;
        }
        self.status = EngineStatus {
            phase: Phase::Processing,
            message: "Transcribing locally…".into(),
            elapsed_ms: self
                .started_at
                .map(|time| time.elapsed().as_millis())
                .unwrap_or(0),
            captured_samples,
            sample_rate,
            ..EngineStatus::default()
        };
        true
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn complete(
        &mut self,
        text: String,
        asr_ms: u128,
        cleanup_ms: u128,
        total_ms: u128,
        message: String,
    ) {
        self.status = EngineStatus {
            phase: Phase::Complete,
            message,
            elapsed_ms: total_ms,
            last_text: Some(text),
            asr_ms,
            cleanup_ms,
            ..EngineStatus::default()
        };
        self.started_at = None;
    }

    pub fn fail(&mut self, message: impl Into<String>) {
        self.status.phase = Phase::Error;
        self.status.message = message.into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforces_valid_recording_transitions() {
        let mut pipeline = Pipeline::default();
        assert!(pipeline.begin());
        assert!(!pipeline.begin());
        assert!(pipeline.processing(48_000, 48_000));
        assert!(!pipeline.processing(0, 0));
        pipeline.reset();
        assert_eq!(pipeline.status.phase, Phase::Idle);
    }
}
