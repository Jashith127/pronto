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
                let mut start = make_cue(true);
                let mut finish = make_cue(false);
                while let Ok(command) = receiver.recv() {
                    match command {
                        SoundCommand::Start(completed) => {
                            let _ = completed.send(play_pcm(&mut start));
                        }
                        SoundCommand::Finish => {
                            let _ = play_pcm(&mut finish);
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

    pub fn finish(&self) {
        let _ = self.sender.send(SoundCommand::Finish);
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

fn make_cue(starts: bool) -> Vec<i16> {
    let duration_ms = if starts { 112 } else { 88 };
    let sample_count = (SAMPLE_RATE * duration_ms / 1_000) as usize;
    let mut samples = Vec::with_capacity(sample_count);
    let mut phase = 0.0f32;
    for index in 0..sample_count {
        let progress = index as f32 / sample_count as f32;
        let (from, to) = if starts {
            (510.0, 720.0)
        } else {
            (650.0, 460.0)
        };
        let frequency = from + (to - from) * progress;
        phase += TAU * frequency / SAMPLE_RATE as f32;
        let attack = (progress / 0.035).min(1.0);
        let release = (1.0 - progress).powf(1.55);
        let envelope = attack * release;
        let level = if starts { 0.19 } else { 0.14 };
        let fundamental = phase.sin() * level;
        let warm_partial = (phase * 1.5).sin() * if starts { 0.028 } else { 0.022 };
        let sample = ((fundamental + warm_partial) * envelope).clamp(-1.0, 1.0);
        samples.push((sample * i16::MAX as f32) as i16);
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
        assert!(start.len() < 6_000);
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
    #[ignore = "plays real sounds through the interactive Windows output device"]
    fn real_start_and_finish_cues_play() {
        let mut start = make_cue(true);
        let mut finish = make_cue(false);
        play_pcm(&mut start).expect("start cue should play");
        std::thread::sleep(Duration::from_millis(120));
        play_pcm(&mut finish).expect("finish cue should play");
    }
}
