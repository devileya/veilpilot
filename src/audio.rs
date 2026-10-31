//! Microphone (and, on Windows, system-audio loopback) capture.
//!
//! Samples are downmixed to mono in the audio callback and buffered. A segment
//! is cut when the speaker pauses or a length limit is reached, by swapping
//! the buffer out, so no audio is lost at the seams. Each segment is resampled to 16 kHz mono 16-bit WAV, which
//! whisper.cpp reads natively and every OpenAI-compatible endpoint accepts —
//! no ffmpeg needed.

use std::f64::consts::PI;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, anyhow};
use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};

pub const TARGET_RATE: u32 = 16_000;
/// Whisper gives nonsense on fragments; a tap on the hotkey is not speech.
const MIN_SECONDS: f64 = 0.4;

/// Voice activity, judged frame by frame (30 ms) against an adaptive noise
/// floor. Deliberately simple: it only has to tell "someone is talking" from
/// "room tone", and err towards talking so quiet speech is never dropped.
struct Vad {
    frame: usize,
    floor: f32,
    /// Consecutive loud frames; a blip shorter than three is not speech.
    run: usize,
    /// Speech frames in the current segment.
    speech_frames: usize,
    /// Sample index just past the last speech frame in the current segment.
    last_speech_end: Option<usize>,
}

/// Speech has to rise this far above the room's noise floor...
const SPEECH_OVER_FLOOR: f32 = 3.0;
/// ...and above this absolute level (about -50 dBFS).
const SPEECH_MIN_RMS: f32 = 0.003;
const MIN_SPEECH_RUN: usize = 3;

impl Vad {
    fn new(rate: u32) -> Self {
        Self {
            frame: (rate as usize * 30 / 1000).max(1),
            floor: 0.002,
            run: 0,
            speech_frames: 0,
            last_speech_end: None,
        }
    }

    fn frame(&mut self, samples: &[f32], end: usize) {
        let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
        let loud = rms > (self.floor * SPEECH_OVER_FLOOR).max(SPEECH_MIN_RMS);
        if rms < self.floor {
            self.floor = rms.max(1e-5);
        } else if !loud {
            self.floor = self.floor * 0.95 + rms * 0.05;
        } else {
            // A noise source that switches on (a fan) is learned within
            // tens of seconds instead of counting as speech forever.
            self.floor *= 1.001;
        }
        self.run = if loud { self.run + 1 } else { 0 };
        if self.run >= MIN_SPEECH_RUN {
            self.speech_frames += 1;
            self.last_speech_end = Some(end);
        }
    }

    fn reset_segment(&mut self) {
        self.run = 0;
        self.speech_frames = 0;
        self.last_speech_end = None;
    }
}

struct Capture {
    samples: Vec<f32>,
    rate: u32,
    error: Option<String>,
    /// How far into `samples` the VAD has looked.
    analysed: usize,
    vad: Vad,
}

impl Capture {
    fn new(rate: u32) -> Self {
        Self { samples: Vec::new(), rate, error: None, analysed: 0, vad: Vad::new(rate) }
    }

    fn analyse(&mut self) {
        let frame = self.vad.frame;
        while self.analysed + frame <= self.samples.len() {
            let end = self.analysed + frame;
            let (vad, samples) = (&mut self.vad, &self.samples);
            vad.frame(&samples[self.analysed..end], end);
            self.analysed = end;
        }
    }

    fn seconds(&self) -> f64 {
        self.samples.len() as f64 / self.rate.max(1) as f64
    }

    /// Seconds since the last speech in this segment; `None` if there was none.
    fn trailing_silence(&self) -> Option<f64> {
        self.vad.last_speech_end.map(|end| (self.analysed - end) as f64 / self.rate.max(1) as f64)
    }

    fn take(&mut self) -> (Vec<f32>, u32) {
        self.analysed = 0;
        self.vad.reset_segment();
        (std::mem::take(&mut self.samples), self.rate)
    }

    /// Drop silence older than `keep` seconds, so a long quiet stretch never
    /// piles up in memory or reaches the transcriber.
    fn trim_silence(&mut self, keep: f64) {
        let keep = (keep * self.rate as f64) as usize;
        if self.vad.speech_frames > 0 || self.analysed <= keep {
            return;
        }
        let drop = self.analysed - keep;
        self.samples.drain(..drop);
        self.analysed -= drop;
    }
}

/// When a segment is cut and sent to transcription while recording.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SegmentPolicy {
    /// Cut after this much silence following speech; 0 turns pause cuts off.
    pub pause_seconds: f64,
    /// Cut when a segment gets this long regardless; 0 means no limit.
    pub max_seconds: f64,
}

/// A stretch of recording, one mono buffer per source.
pub struct Segment {
    pub parts: Vec<(Vec<f32>, u32)>,
    /// Whether anyone spoke in it. Silent segments are not transcribed:
    /// Whisper invents text for silence.
    pub speech: bool,
}

struct Source {
    _stream: Option<cpal::Stream>,
    capture: Arc<Mutex<Capture>>,
}

pub struct Recorder {
    sources: Vec<Source>,
}

pub fn input_devices() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices().map(|devices| devices.filter_map(|d| d.name().ok()).collect()).unwrap_or_default()
}

fn open(device: &cpal::Device) -> anyhow::Result<Source> {
    let config = device.default_input_config().context("reading the input format")?;
    let channels = config.channels().max(1) as usize;
    let rate = config.sample_rate().0;
    let capture = Arc::new(Mutex::new(Capture::new(rate)));
    let stream_config: cpal::StreamConfig = config.clone().into();

    macro_rules! build {
        ($t:ty, $to_f32:expr) => {{
            let sink = capture.clone();
            let errors = capture.clone();
            device.build_input_stream::<$t, _, _>(
                &stream_config,
                move |data: &[$t], _| {
                    let to_f32 = $to_f32;
                    if let Ok(mut c) = sink.lock() {
                        c.samples.extend(
                            data.chunks(channels)
                                .map(|frame| frame.iter().map(|s| to_f32(*s)).sum::<f32>() / frame.len() as f32),
                        );
                    }
                },
                move |err| {
                    if let Ok(mut c) = errors.lock() {
                        c.error = Some(err.to_string());
                    }
                },
                None,
            )
        }};
    }

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build!(f32, |s: f32| s),
        cpal::SampleFormat::I16 => build!(i16, |s: i16| s as f32 / 32768.0),
        cpal::SampleFormat::U16 => build!(u16, |s: u16| (s as f32 - 32768.0) / 32768.0),
        cpal::SampleFormat::I32 => build!(i32, |s: i32| s as f32 / 2_147_483_648.0),
        cpal::SampleFormat::F64 => build!(f64, |s: f64| s as f32),
        other => return Err(anyhow!("unsupported sample format {other:?}")),
    }
    .context("opening the input stream")?;
    stream.play().context("starting the input stream")?;
    Ok(Source { _stream: Some(stream), capture })
}

impl Recorder {
    /// `device_name` empty means the system default input. Returns a warning
    /// when system audio was asked for but is not available.
    pub fn start(device_name: &str, system_audio: bool) -> anyhow::Result<(Recorder, Option<String>)> {
        let host = cpal::default_host();
        let device = if device_name.is_empty() {
            host.default_input_device()
        } else {
            host.input_devices()
                .ok()
                .and_then(|mut all| all.find(|d| d.name().ok().as_deref() == Some(device_name)))
                .or_else(|| host.default_input_device())
        }
        .ok_or_else(|| anyhow!("No microphone found."))?;

        let mut sources = vec![open(&device)?];
        let mut warning = None;

        if system_audio {
            // WASAPI records an output device in loopback mode. macOS gives
            // apps no system-audio tap, so the honest answer there is a
            // virtual device (BlackHole) selected as the input — not a
            // silent, half-broken recording.
            let loopback = if cfg!(windows) {
                host.default_output_device().ok_or_else(|| anyhow!("no output device")).and_then(|d| open(&d))
            } else {
                Err(anyhow!("no system audio tap"))
            };
            match loopback {
                Ok(source) => sources.push(source),
                Err(_) => {
                    warning = Some(
                        "System audio is not available here — recording the microphone only. On macOS, install a loopback device such as BlackHole and select it as the input."
                            .into(),
                    )
                }
            }
        }
        Ok((Recorder { sources }, warning))
    }

    /// A recorder fed from samples instead of a device, delivered in real
    /// time (times `speed`) — the dev control channel's stand-in microphone.
    pub fn simulated(samples: Vec<f32>, rate: u32, speed: f64) -> Recorder {
        let capture = Arc::new(Mutex::new(Capture::new(rate)));
        let sink = capture.clone();
        std::thread::spawn(move || {
            let block = (rate as usize / 50).max(1); // 20 ms
            let interval = 0.02 / speed.max(0.01);
            let start = std::time::Instant::now();
            // Like a live microphone, it keeps delivering (silence) after the
            // file ends, until the recorder is dropped.
            let silence = vec![0.0f32; block];
            let mut chunks = samples.chunks(block);
            for n in 1u32.. {
                // Scheduled against the start time, so sleeps never drift.
                let due = start + std::time::Duration::from_secs_f64(interval * n as f64);
                if let Some(wait) = due.checked_duration_since(std::time::Instant::now()) {
                    std::thread::sleep(wait);
                }
                if Arc::strong_count(&sink) == 1 {
                    return;
                }
                let chunk = chunks.next().unwrap_or(&silence);
                match sink.lock() {
                    Ok(mut c) => c.samples.extend_from_slice(chunk),
                    Err(_) => return,
                }
            }
        });
        Recorder { sources: vec![Source { _stream: None, capture }] }
    }

    /// A stream error reported by the audio callback, if any.
    pub fn error(&self) -> Option<String> {
        self.sources.iter().find_map(|s| s.capture.lock().ok().and_then(|c| c.error.clone()))
    }

    /// Look at what arrived since the last call and cut a segment if the
    /// policy says so. Cheap enough to call from the UI thread a few times a
    /// second; the heavy work is [`encode_segment`].
    pub fn poll(&self, policy: SegmentPolicy) -> Option<Segment> {
        let mut captures: Vec<_> = self.sources.iter().map(|s| s.capture.lock().unwrap()).collect();
        for c in captures.iter_mut() {
            c.analyse();
        }
        let speech = captures.iter().any(|c| c.vad.speech_frames > 0);
        let longest = captures.iter().map(|c| c.seconds()).fold(0.0, f64::max);

        // A pause means every source has gone quiet: on Windows the other
        // side of the call may still be talking while you are not.
        let paused = policy.pause_seconds > 0.0
            && speech
            && captures.iter().all(|c| c.trailing_silence().is_none_or(|s| s >= policy.pause_seconds));
        let too_long = policy.max_seconds > 0.0 && longest >= policy.max_seconds;

        if paused || too_long {
            return Some(Segment { parts: captures.iter_mut().map(|c| c.take()).collect(), speech });
        }
        if !speech && policy.pause_seconds > 0.0 {
            for c in captures.iter_mut() {
                c.trim_silence(1.0);
            }
        }
        None
    }

    /// Everything left, for when recording stops.
    pub fn finish(&self) -> Segment {
        let mut captures: Vec<_> = self.sources.iter().map(|s| s.capture.lock().unwrap()).collect();
        for c in captures.iter_mut() {
            c.analyse();
        }
        let speech = captures.iter().any(|c| c.vad.speech_frames > 0);
        Segment { parts: captures.iter_mut().map(|c| c.take()).collect(), speech }
    }
}

/// Resample each source to 16 kHz, mix, and encode. `None` when the segment is
/// too short to be speech.
pub fn encode_segment(parts: Vec<(Vec<f32>, u32)>) -> Option<Vec<u8>> {
    let mut mixed: Vec<f32> = Vec::new();
    for (samples, rate) in parts {
        let resampled = resample(&samples, rate, TARGET_RATE);
        if mixed.len() < resampled.len() {
            mixed.resize(resampled.len(), 0.0);
        }
        for (m, s) in mixed.iter_mut().zip(resampled) {
            *m += s;
        }
    }
    if (mixed.len() as f64) < MIN_SECONDS * TARGET_RATE as f64 {
        return None;
    }
    Some(encode_wav(&mixed, TARGET_RATE))
}

/// Windowed-sinc resampler. Downsampling low-passes at the new Nyquist first,
/// so 48 kHz speech does not alias into the band Whisper listens to.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() || from == 0 {
        return input.to_vec();
    }
    const PHASES: usize = 256;
    let ratio = to as f64 / from as f64;
    let cutoff = ratio.min(1.0) * 0.95;
    let half_width = (8.0 / cutoff).ceil() as isize;
    let taps = (2 * half_width) as usize;

    // Polyphase table: the kernel depends only on the fractional position of
    // the output sample, so it is computed once per phase instead of per tap.
    let mut table = vec![0f32; PHASES * taps];
    for phase in 0..PHASES {
        let frac = phase as f64 / PHASES as f64;
        let row = &mut table[phase * taps..(phase + 1) * taps];
        let mut sum = 0.0;
        for (j, slot) in row.iter_mut().enumerate() {
            // Tap j sits at input index base - half_width + 1 + j.
            let x = frac + half_width as f64 - 1.0 - j as f64;
            let arg = x * cutoff;
            let sinc = if arg.abs() < 1e-9 { 1.0 } else { (PI * arg).sin() / (PI * arg) };
            // Blackman window over the kernel span.
            let t = (x / half_width as f64 + 1.0) / 2.0;
            let window = if (0.0..=1.0).contains(&t) {
                0.42 - 0.5 * (2.0 * PI * t).cos() + 0.08 * (4.0 * PI * t).cos()
            } else {
                0.0
            };
            *slot = (sinc * window) as f32;
            sum += sinc * window;
        }
        // Unity gain at DC for every phase.
        for slot in row.iter_mut() {
            *slot = (*slot as f64 / sum) as f32;
        }
    }

    let out_len = (input.len() as f64 * ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let center = i as f64 / ratio;
        let base = center.floor() as isize;
        let phase = (((center - base as f64) * PHASES as f64) as usize).min(PHASES - 1);
        let row = &table[phase * taps..(phase + 1) * taps];
        let start = base - half_width + 1;
        let mut acc = 0f32;
        if start >= 0 && (start as usize + taps) <= input.len() {
            let window = &input[start as usize..start as usize + taps];
            for (s, w) in window.iter().zip(row) {
                acc += s * w;
            }
        } else {
            // Edges: treat samples outside the recording as silence.
            for (j, w) in row.iter().enumerate() {
                let k = start + j as isize;
                if k >= 0 && (k as usize) < input.len() {
                    acc += input[k as usize] * w;
                }
            }
        }
        out.push(acc);
    }
    out
}

pub fn encode_wav(samples: &[f32], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM header size
    out.extend_from_slice(&1u16.to_le_bytes()); // format: PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        let s = s.clamp(-1.0, 1.0);
        let v = if s < 0.0 { s * 32768.0 } else { s * 32767.0 };
        out.extend_from_slice(&(v as i16).to_le_bytes());
    }
    out
}

/// Read 16-bit PCM WAV back to samples (tests and the self-test harness).
pub fn decode_wav(bytes: &[u8]) -> Option<(Vec<f32>, u32, u16)> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12;
    let (mut rate, mut channels, mut bits) = (0u32, 0u16, 0u16);
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().ok()?) as usize;
        let body = bytes.get(pos + 8..(pos + 8 + len).min(bytes.len()))?;
        if id == b"fmt " && body.len() >= 16 {
            channels = u16::from_le_bytes([body[2], body[3]]);
            rate = u32::from_le_bytes(body[4..8].try_into().ok()?);
            bits = u16::from_le_bytes([body[14], body[15]]);
        } else if id == b"data" && bits == 16 {
            let samples =
                body.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0).collect::<Vec<_>>();
            let ch = channels.max(1) as usize;
            let mono = samples.chunks(ch).map(|f| f.iter().sum::<f32>() / f.len() as f32).collect();
            return Some((mono, rate, channels));
        }
        pos += 8 + len + (len & 1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f64, rate: u32, seconds: f64) -> Vec<f32> {
        (0..(rate as f64 * seconds) as usize)
            .map(|i| (2.0 * PI * freq * i as f64 / rate as f64).sin() as f32 * 0.5)
            .collect()
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
    }

    #[test]
    fn resample_keeps_speech_band_and_removes_alias() {
        let speech = resample(&tone(440.0, 48_000, 1.0), 48_000, 16_000);
        assert_eq!(speech.len(), 16_000);
        assert!((rms(&speech[1000..15000]) - 0.3535).abs() < 0.02, "440 Hz passes");
        // 12 kHz cannot exist at 16 kHz; without filtering it would fold to 4 kHz.
        let alias = resample(&tone(12_000.0, 48_000, 1.0), 48_000, 16_000);
        assert!(rms(&alias[1000..15000]) < 0.02, "12 kHz is filtered out");
    }

    #[test]
    fn wav_round_trip() {
        let wav = encode_wav(&[0.0, 0.5, -0.5, 1.0], 16_000);
        assert_eq!(wav.len(), 44 + 8);
        let (back, rate, ch) = decode_wav(&wav).unwrap();
        assert_eq!((rate, ch), (16_000, 1));
        assert!((back[1] - 0.5).abs() < 1e-3);
        assert!((back[2] + 0.5).abs() < 1e-3);
    }

    #[test]
    fn short_segments_are_dropped_and_sources_mix() {
        assert!(encode_segment(vec![(vec![0.1; 1000], 16_000)]).is_none());
        let wav = encode_segment(vec![(vec![0.25; 16_000], 16_000), (vec![0.25; 8_000], 16_000)]).unwrap();
        let (back, _, _) = decode_wav(&wav).unwrap();
        assert_eq!(back.len(), 16_000);
        assert!((back[100] - 0.5).abs() < 1e-3);
        assert!((back[12_000] - 0.25).abs() < 1e-3);
    }

    /* ---------- segmentation ---------- */

    const RATE: u32 = 16_000;

    fn manual() -> (Recorder, Arc<Mutex<Capture>>) {
        let capture = Arc::new(Mutex::new(Capture::new(RATE)));
        (Recorder { sources: vec![Source { _stream: None, capture: capture.clone() }] }, capture)
    }

    /// Room tone: deterministic low-level noise.
    fn room(seconds: f64) -> Vec<f32> {
        let mut x: u32 = 12345;
        (0..(RATE as f64 * seconds) as usize)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                ((x >> 16) as f32 / 65536.0 - 0.5) * 0.002
            })
            .collect()
    }

    /// Voiced speech stand-in: a 180 Hz tone with a syllable-rate envelope.
    fn voice(seconds: f64) -> Vec<f32> {
        (0..(RATE as f64 * seconds) as usize)
            .map(|i| {
                let t = i as f64 / RATE as f64;
                ((2.0 * PI * 180.0 * t).sin() * (0.6 + 0.4 * (2.0 * PI * 4.0 * t).sin()) * 0.08) as f32
            })
            .collect()
    }

    /// Feed `audio` in 100 ms steps, polling after each; returns the cuts
    /// with the time (s) each happened.
    fn run(audio: &[f32], policy: SegmentPolicy) -> (Vec<(f64, Segment)>, Recorder) {
        let (rec, cap) = manual();
        let step = RATE as usize / 10;
        let mut cuts = Vec::new();
        for (n, chunk) in audio.chunks(step).enumerate() {
            cap.lock().unwrap().samples.extend_from_slice(chunk);
            if let Some(seg) = rec.poll(policy) {
                cuts.push(((n + 1) as f64 / 10.0, seg));
            }
        }
        (cuts, rec)
    }

    fn secs(seg: &Segment) -> f64 {
        seg.parts[0].0.len() as f64 / RATE as f64
    }

    const PAUSES: SegmentPolicy = SegmentPolicy { pause_seconds: 1.5, max_seconds: 60.0 };

    #[test]
    fn a_pause_cuts_the_segment_right_away() {
        let audio = [room(0.5), voice(3.0), room(2.0), voice(2.0), room(0.3)].concat();
        let (cuts, rec) = run(&audio, PAUSES);
        assert_eq!(cuts.len(), 1, "one pause, one cut");
        let (at, seg) = &cuts[0];
        assert!(seg.speech);
        // Speech ended at 3.5 s; the cut lands about 1.5 s later.
        assert!((4.9..=5.3).contains(at), "cut at {at}");
        assert!((4.9..=5.3).contains(&secs(seg)));
        let rest = rec.finish();
        assert!(rest.speech, "the second sentence is still there on stop");
    }

    #[test]
    fn a_short_breath_does_not_cut() {
        let audio = [voice(2.0), room(0.8), voice(2.0), room(0.5)].concat();
        let (cuts, rec) = run(&audio, PAUSES);
        assert!(cuts.is_empty());
        assert!((5.0..=5.4).contains(&(rec.finish().parts[0].0.len() as f64 / RATE as f64)));
    }

    #[test]
    fn continuous_talk_is_cut_at_the_length_limit() {
        let audio = voice(25.0);
        let (cuts, _) = run(&audio, SegmentPolicy { pause_seconds: 1.5, max_seconds: 10.0 });
        assert_eq!(cuts.len(), 2);
        assert!(cuts.iter().all(|(_, s)| s.speech && (secs(s) - 10.0).abs() < 0.15));
    }

    #[test]
    fn silence_is_trimmed_and_never_transcribed() {
        let (cuts, rec) = run(&room(30.0), PAUSES);
        assert!(cuts.is_empty());
        let rest = rec.finish();
        assert!(!rest.speech);
        assert!(secs(&rest) <= 1.2, "silence does not pile up: {}", secs(&rest));
    }

    #[test]
    fn a_click_is_not_speech() {
        let audio = [room(1.0), voice(0.05), room(3.0)].concat();
        let (cuts, rec) = run(&audio, PAUSES);
        assert!(cuts.is_empty());
        assert!(!rec.finish().speech);
    }

    #[test]
    fn time_only_chunks_still_work() {
        let audio = [voice(4.0), room(4.0), voice(4.0)].concat();
        let (cuts, _) = run(&audio, SegmentPolicy { pause_seconds: 0.0, max_seconds: 5.0 });
        assert_eq!(cuts.len(), 2);
        assert!(cuts[0].1.speech && (secs(&cuts[0].1) - 5.0).abs() < 0.15);
    }
}
