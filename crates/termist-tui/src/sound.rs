//! The two Istanbul sounds, made by code: a ferry's horn when an agent waits for you,
//! a seagull when one is done. The parameters are the approved prototype's
//! (`reference/sounds-prototype.py`); the noise comes from a fixed seed, so a sound is
//! the same every time.
use std::f64::consts::PI;
use std::path::{Path, PathBuf};

pub const SAMPLE_RATE: u32 = 44_100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sound {
    /// Vapur düdüğü: an agent waits for you.
    Vapur,
    /// Martı: an agent is done.
    Marti,
}

impl Sound {
    pub fn name(self) -> &'static str {
        match self {
            Sound::Vapur => "vapur",
            Sound::Marti => "marti",
        }
    }

    pub fn from_name(name: &str) -> Option<Sound> {
        match name {
            "vapur" => Some(Sound::Vapur),
            "marti" | "martı" => Some(Sound::Marti),
            _ => None,
        }
    }

    pub fn samples(self) -> Vec<f64> {
        match self {
            Sound::Vapur => vapur(),
            Sound::Marti => marti(),
        }
    }
}

/// White noise from a fixed seed, in -1..1.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

/// A linear attack of `a` seconds and release of `r`, over `dur`.
fn envelope(t: f64, dur: f64, a: f64, r: f64) -> f64 {
    if t < a {
        t / a
    } else if t > dur - r {
        ((dur - t) / r).max(0.0)
    } else {
        1.0
    }
}

/// Ship horn: two detuned low notes a minor third apart, odd-heavy harmonics, a slow
/// attack, gentle vibrato and a little breath noise. 1.1 s.
fn vapur() -> Vec<f64> {
    vapur_with(0.05)
}

fn vapur_with(breath: f64) -> Vec<f64> {
    let dur = 1.1;
    let notes = [(116.5, 1.0), (117.3, 0.8), (138.6, 0.7), (139.4, 0.5)];
    let mut phases = [0.0f64; 4];
    let mut noise = Noise(7);
    (0..(SAMPLE_RATE as f64 * dur) as usize)
        .map(|i| {
            let t = i as f64 / SAMPLE_RATE as f64;
            let vibrato = 1.0 + 0.004 * (2.0 * PI * 5.2 * t).sin();
            let mut s = 0.0;
            for ((f, amp), ph) in notes.iter().zip(&mut phases) {
                *ph += 2.0 * PI * f * vibrato / SAMPLE_RATE as f64;
                s += amp
                    * [1.0, 2.0, 3.0, 5.0, 7.0]
                        .iter()
                        .map(|h| (h * *ph).sin() / h)
                        .sum::<f64>();
            }
            s += breath * noise.next();
            s * envelope(t, dur, 0.12, 0.35)
        })
        .collect()
}

/// Seagull: two short "kyaa" calls, the pitch gliding down from 2.3 kHz, a nasal FM
/// timbre. 0.9 s.
fn marti() -> Vec<f64> {
    marti_with(0.08)
}

fn marti_with(rasp: f64) -> Vec<f64> {
    let dur = 0.9;
    let calls = [(0.0, 0.32), (0.42, 0.40)];
    let mut ph = 0.0f64;
    let mut noise = Noise(7);
    (0..(SAMPLE_RATE as f64 * dur) as usize)
        .map(|i| {
            let t = i as f64 / SAMPLE_RATE as f64;
            let mut s = 0.0;
            for (start, length) in calls {
                let u = t - start;
                if (0.0..length).contains(&u) {
                    let x = u / length;
                    let f = 2300.0 - 900.0 * x + 250.0 * (PI * x).sin();
                    ph += 2.0 * PI * f / SAMPLE_RATE as f64;
                    let modulation = (ph * 0.5).sin() * 1.8;
                    let shape = (PI * x).sin();
                    s += (ph + modulation).sin() * shape.powf(0.6);
                    s += rasp * noise.next() * shape;
                }
            }
            s
        })
        .collect()
}

/// A mono 16-bit WAV of `samples`, its loudest point at 80 % of full scale.
pub fn wav(samples: &[f64]) -> Vec<u8> {
    let peak = samples.iter().fold(0.0f64, |m, s| m.max(s.abs()));
    let peak = if peak > 0.0 { peak } else { 1.0 };
    let data_len = samples.len() as u32 * 2;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        let v = (s / peak * 0.8 * 32767.0) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// `sound` as a WAV file in `dir`, written the first time it is needed.
pub fn file(dir: &Path, sound: Sound) -> std::io::Result<PathBuf> {
    let path = dir.join(format!("{}.wav", sound.name()));
    let bytes = wav(&sound.samples());
    let current = std::fs::metadata(&path).is_ok_and(|m| m.len() == bytes.len() as u64);
    if !current {
        std::fs::create_dir_all(dir)?;
        std::fs::write(&path, bytes)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sounds_last_as_long_as_the_prototypes() {
        assert_eq!(vapur().len(), 48_510);
        assert_eq!(marti().len(), 39_690);
    }

    #[test]
    fn the_horn_swells_and_fades_and_the_gull_calls_twice() {
        let horn = vapur();
        let loudness = |s: &[f64]| s.iter().map(|x| x * x).sum::<f64>() / s.len() as f64;
        let start = loudness(&horn[..441]);
        let middle = loudness(&horn[20_000..24_000]);
        assert!(middle > start * 10.0, "a slow attack");
        assert!(horn.last().unwrap().abs() < 0.05, "it fades to nothing");
        let gull = marti();
        let gap = loudness(&gull[(0.34 * 44_100.0) as usize..(0.40 * 44_100.0) as usize]);
        let call = loudness(&gull[(0.10 * 44_100.0) as usize..(0.20 * 44_100.0) as usize]);
        assert_eq!(gap, 0.0, "silence between the calls");
        assert!(call > 0.1);
    }

    #[test]
    fn the_same_sound_every_time() {
        assert_eq!(vapur(), vapur());
    }

    #[test]
    fn a_wav_is_a_proper_riff_file_at_80_percent() {
        let bytes = wav(&[0.0, 0.5, -1.0]);
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            44_100
        );
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 6);
        let last = i16::from_le_bytes(bytes[48..50].try_into().unwrap());
        assert_eq!(last, -26213, "the peak at 80 %");
    }

    #[test]
    fn files_are_written_once() {
        let tmp = tempfile::tempdir().unwrap();
        let path = file(tmp.path(), Sound::Marti).unwrap();
        let written = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(file(tmp.path(), Sound::Marti).unwrap(), path);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            written
        );
    }

    /// The prototype's own numbers, its noise left out (Python's generator differs).
    #[test]
    fn without_noise_the_sounds_are_the_prototypes_sample_for_sample() {
        let horn = vapur_with(0.0);
        let expected = [
            0.032338528,
            0.018555984,
            -0.954133083,
            -0.692306559,
            0.000164024,
        ];
        for (i, want) in [100, 5000, 20_000, 30_000, horn.len() - 1]
            .into_iter()
            .zip(expected)
        {
            assert!((horn[i] - want).abs() < 1e-8, "vapur[{i}] = {}", horn[i]);
        }
        let gull = marti_with(0.0);
        let expected = [0.033797195, -0.025830466, -0.394395190, -0.245676994, 0.0];
        for (i, want) in [100, 5000, 20_000, 30_000, gull.len() - 1]
            .into_iter()
            .zip(expected)
        {
            assert!((gull[i] - want).abs() < 1e-8, "marti[{i}] = {}", gull[i]);
        }
    }
}
