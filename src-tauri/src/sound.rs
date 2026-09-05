use std::f32::consts::TAU;
use std::mem::size_of;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use windows::core::PSTR;
use windows::Win32::Media::Audio::{
    waveOutClose, waveOutOpen, waveOutPrepareHeader, waveOutReset, waveOutUnprepareHeader,
    waveOutWrite, CALLBACK_NULL, HWAVEOUT, WAVEFORMATEX, WAVEHDR, WAVE_FORMAT_PCM, WAVE_MAPPER,
    WHDR_DONE,
};

const SAMPLE_RATE: u32 = 44_100;

enum SoundCommand {
    Start(mpsc::Sender<Result<(), String>>),
    Finish(mpsc::Sender<Result<(), String>>),
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
                let mut start = make_cue(true);
                let mut finish = make_cue(false);
                while let Ok(command) = receiver.recv() {
                    match command {
                        SoundCommand::Start(completed) => {
                            let _ = completed.send(play_pcm(&mut start));
                        }
                        SoundCommand::Finish(completed) => {
                            let _ = completed.send(play_pcm(&mut finish));
                        }
                    }
                }
            })
            .expect("failed to start dictation sound thread");
        Self { sender }
    }

    pub fn start_and_wait(&self) -> Result<(), String> {
        let (completed, response) = mpsc::channel();
        self.sender
            .send(SoundCommand::Start(completed))
            .map_err(|_| "Dictation sound thread stopped".to_string())?;
        response
            .recv_timeout(Duration::from_millis(500))
            .map_err(|_| "The recording start cue timed out".to_string())?
    }

    pub fn finish_and_wait(&self) -> Result<(), String> {
        let (completed, response) = mpsc::channel();
        self.sender
            .send(SoundCommand::Finish(completed))
            .map_err(|_| "Dictation sound thread stopped".to_string())?;
        response
            .recv_timeout(Duration::from_millis(500))
            .map_err(|_| "The recording stop cue timed out".to_string())?
    }
}

fn play_pcm(samples: &mut [i16]) -> Result<(), String> {
    let format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM as u16,
        nChannels: 1,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * 2,
        nBlockAlign: 2,
        wBitsPerSample: 16,
        cbSize: 0,
    };
    let mut output = HWAVEOUT::default();
    let opened = unsafe {
        waveOutOpen(
            Some(&mut output),
            WAVE_MAPPER,
            &format,
            None,
            None,
            CALLBACK_NULL,
        )
    };
    if opened != 0 {
        return Err(format!(
            "Windows could not open the sound output ({opened})"
        ));
    }

    let mut header = WAVEHDR {
        lpData: PSTR(samples.as_mut_ptr().cast::<u8>()),
        dwBufferLength: std::mem::size_of_val(samples) as u32,
        ..Default::default()
    };
    let header_size = size_of::<WAVEHDR>() as u32;
    let prepared = unsafe { waveOutPrepareHeader(output, &mut header, header_size) };
    if prepared != 0 {
        let _ = unsafe { waveOutClose(output) };
        return Err(format!(
            "Windows could not prepare the recording cue ({prepared})"
        ));
    }
    let written = unsafe { waveOutWrite(output, &mut header, header_size) };
    if written != 0 {
        let _ = unsafe { waveOutUnprepareHeader(output, &mut header, header_size) };
        let _ = unsafe { waveOutClose(output) };
        return Err(format!(
            "Windows could not play the recording cue ({written})"
        ));
    }

    let deadline = Instant::now() + Duration::from_millis(450);
    while header.dwFlags & WHDR_DONE == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(4));
    }
    let completed = header.dwFlags & WHDR_DONE != 0;
    if !completed {
        let _ = unsafe { waveOutReset(output) };
    }
    let _ = unsafe { waveOutUnprepareHeader(output, &mut header, header_size) };
    let _ = unsafe { waveOutClose(output) };
    if completed {
        Ok(())
    } else {
        Err("The recording cue did not finish playing".into())
    }
}

/// Modern two-tone UI blips: discrete low notes (A3 <-> E4) with a fast
/// attack and exponential decay. No pitch glide, so nothing chirps or
/// bubbles; direction alone tells start (ascending) from stop
/// (descending). Fundamentals stay above 200 Hz to survive narrowband
/// Bluetooth hands-free links.
fn make_cue(starts: bool) -> Vec<i16> {
    const A3: f32 = 220.0;
    const E4: f32 = 329.63;
    // (frequency, milliseconds, peak level)
    let notes: [(f32, u32, f32); 2] = if starts {
        [(A3, 48, 0.34), (E4, 58, 0.34)]
    } else {
        [(E4, 44, 0.30), (A3, 52, 0.30)]
    };
    let mut samples = Vec::new();
    if starts {
        // Endpoints in low-power idle (notably Bluetooth headsets) can
        // swallow the first tens of milliseconds after opening. Leading
        // silence wakes the route so the audible notes arrive complete.
        samples.extend(std::iter::repeat_n(
            0,
            (SAMPLE_RATE * 70 / 1_000) as usize,
        ));
    }
    for (note_index, &(frequency, duration_ms, target_peak)) in notes.iter().enumerate() {
        if note_index > 0 {
            // 6 ms of silence between notes keeps the two tones distinct.
            samples.extend(std::iter::repeat_n(
                0,
                (SAMPLE_RATE * 6 / 1_000) as usize,
            ));
        }
        let note_samples = (SAMPLE_RATE * duration_ms / 1_000) as usize;
        // Synthesize unscaled first so the note can be normalized to its
        // exact target peak: harmonic phase alignment otherwise makes the
        // final loudness hard to predict.
        let mut note = Vec::with_capacity(note_samples);
        let mut phase = 0.0f32;
        for index in 0..note_samples {
            let time = index as f32 / SAMPLE_RATE as f32;
            let length = note_samples as f32 / SAMPLE_RATE as f32;
            phase += TAU * frequency / SAMPLE_RATE as f32;
            let attack = (time / 0.004).min(1.0);
            let decay = (-3.2 * time / length).exp();
            let tone = phase.sin() * 0.72
                + (phase * 2.0).sin() * 0.20
                + (phase * 3.0).sin() * 0.08;
            note.push(tone * attack * decay);
        }
        let peak = note
            .iter()
            .map(|sample| sample.abs())
            .max_by(|left, right| left.total_cmp(right))
            .unwrap_or(0.0)
            .max(f32::EPSILON);
        let gain = target_peak / peak;
        samples.extend(
            note.iter()
                .map(|sample| ((sample * gain).clamp(-1.0, 1.0) * i16::MAX as f32) as i16),
        );
    }
    samples
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cues_are_short_audible_pcm() {
        let start = make_cue(true);
        let finish = make_cue(false);
        assert!(start.len() > finish.len());
        assert!(start.len() < 9_000);
        let peak = |cue: &[i16]| {
            cue.iter()
                .map(|sample| sample.unsigned_abs())
                .max()
                .unwrap_or(0)
        };
        assert!(peak(&start) > peak(&finish));
        assert!(peak(&start) > 5_000);
        assert!(peak(&finish) > 3_500);
    }

    #[test]
    fn cues_step_in_opposite_directions() {
        // Zero-crossing rate tracks pitch: the start cue must step up and
        // the stop cue must step down, with a quiet gap between the notes.
        let direction = |cue: &[i16]| {
            let crossings = |half: &[i16]| {
                half.windows(2)
                    .filter(|pair| (pair[0] < 0) != (pair[1] < 0))
                    .count() as f32
                    / half.len() as f32
            };
            let mid = cue.len() / 2;
            crossings(&cue[mid..]) - crossings(&cue[..mid])
        };
        assert!(direction(&make_cue(true)) > 0.0);
        assert!(direction(&make_cue(false)) < 0.0);
    }

    #[test]
    #[ignore = "plays real sounds through the interactive Windows output device"]
    fn real_start_and_finish_cues_play() {
        let mut start = make_cue(true);
        let mut finish = make_cue(false);
        play_pcm(&mut start).expect("start cue should play");
        std::thread::sleep(Duration::from_millis(120));
        play_pcm(&mut finish).expect("finish cue should play");
    }
}
