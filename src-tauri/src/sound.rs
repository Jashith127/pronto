use std::f32::consts::TAU;
use std::ffi::c_void;
use std::sync::mpsc;

const SND_SYNC: u32 = 0x0000;
const SND_NODEFAULT: u32 = 0x0002;
const SND_MEMORY: u32 = 0x0004;

#[link(name = "winmm")]
extern "system" {
    fn PlaySoundW(sound: *const u16, module: *mut c_void, flags: u32) -> i32;
}

enum SoundCommand {
    Start,
    Finish,
}

pub struct SoundController {
    sender: mpsc::Sender<SoundCommand>,
}

impl SoundController {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("pronto-sounds".into())
            .spawn(move || {
                let start = make_cue(true);
                let finish = make_cue(false);
                while let Ok(command) = receiver.recv() {
                    let bytes = match command {
                        SoundCommand::Start => &start,
                        SoundCommand::Finish => &finish,
                    };
                    unsafe {
                        let _ = PlaySoundW(
                            bytes.as_ptr().cast::<u16>(),
                            std::ptr::null_mut(),
                            SND_MEMORY | SND_NODEFAULT | SND_SYNC,
                        );
                    }
                }
            })
            .expect("failed to start dictation sound thread");
        Self { sender }
    }

    pub fn start(&self) {
        let _ = self.sender.send(SoundCommand::Start);
    }

    pub fn finish(&self) {
        let _ = self.sender.send(SoundCommand::Finish);
    }
}

fn make_cue(starts: bool) -> Vec<u8> {
    const SAMPLE_RATE: u32 = 44_100;
    const DURATION_MS: u32 = 82;
    let sample_count = (SAMPLE_RATE * DURATION_MS / 1_000) as usize;
    let mut samples = Vec::with_capacity(sample_count);
    let mut phase = 0.0f32;
    for index in 0..sample_count {
        let progress = index as f32 / sample_count as f32;
        let (from, to) = if starts {
            (610.0, 980.0)
        } else {
            (900.0, 520.0)
        };
        let frequency = from + (to - from) * progress;
        phase += TAU * frequency / SAMPLE_RATE as f32;
        let attack = (progress / 0.08).min(1.0);
        let release = ((1.0 - progress) / 0.38).clamp(0.0, 1.0);
        let envelope = attack * release;
        let overtone = if starts { 0.18 } else { 0.12 } * (phase * 2.0).sin();
        let click = if index < 48 {
            (1.0 - index as f32 / 48.0) * 0.08
        } else {
            0.0
        };
        let sample = ((phase.sin() * 0.24 + overtone + click) * envelope).clamp(-1.0, 1.0);
        samples.push((sample * i16::MAX as f32) as i16);
    }
    pcm_wav(&samples, SAMPLE_RATE)
}

fn pcm_wav(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * std::mem::size_of::<i16>()) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cues_are_short_valid_pcm_waves() {
        for cue in [make_cue(true), make_cue(false)] {
            assert_eq!(&cue[..4], b"RIFF");
            assert_eq!(&cue[8..12], b"WAVE");
            assert_eq!(u32::from_le_bytes(cue[24..28].try_into().unwrap()), 44_100);
            assert!(cue.len() < 12_000);
        }
    }
}
