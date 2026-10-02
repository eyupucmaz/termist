//! termist's own sounds, a seagull (martı) and a cat (kedi), for an agent that waits
//! for you or is done. The recordings are built into the binary as WAV files, so
//! every platform's player can play them.
use std::path::{Path, PathBuf};

/// The martı: mono, 16-bit, 44.1 kHz.
pub const MARTI: &[u8] = include_bytes!("../../../assets/sounds/marti.wav");
/// The kedi, in the same format.
pub const KEDI: &[u8] = include_bytes!("../../../assets/sounds/kedi.wav");

/// One of termist's recordings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recording {
    Marti,
    Kedi,
}

impl Recording {
    pub fn name(self) -> &'static str {
        match self {
            Recording::Marti => "marti",
            Recording::Kedi => "kedi",
        }
    }

    pub fn bytes(self) -> &'static [u8] {
        match self {
            Recording::Marti => MARTI,
            Recording::Kedi => KEDI,
        }
    }
}

/// The recording as a WAV file in `dir`, written the first time it is needed.
pub fn file(dir: &Path, recording: Recording) -> std::io::Result<PathBuf> {
    let path = dir.join(format!("{}.wav", recording.name()));
    let current = std::fs::read(&path).is_ok_and(|b| b == recording.bytes());
    if !current {
        std::fs::create_dir_all(dir)?;
        std::fs::write(&path, recording.bytes())?;
        // Earlier releases also wrote a ferry's horn here.
        let _ = std::fs::remove_file(dir.join("vapur.wav"));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recordings_are_mono_16_bit_wavs() {
        for wav in [MARTI, KEDI] {
            assert_eq!(&wav[..4], b"RIFF");
            assert_eq!(&wav[8..16], b"WAVEfmt ");
            assert_eq!(u16::from_le_bytes([wav[20], wav[21]]), 1, "PCM");
            assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1, "mono");
            assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 44_100);
            assert_eq!(u16::from_le_bytes([wav[34], wav[35]]), 16);
        }
    }

    #[test]
    fn files_are_written_once() {
        let tmp = tempfile::tempdir().unwrap();
        let path = file(tmp.path(), Recording::Kedi).unwrap();
        assert_eq!(path, tmp.path().join("kedi.wav"));
        let written = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(file(tmp.path(), Recording::Kedi).unwrap(), path);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            written
        );
    }

    #[test]
    fn old_sound_files_are_replaced_and_the_horn_removed() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("marti.wav"), b"RIFF old").unwrap();
        std::fs::write(tmp.path().join("vapur.wav"), b"RIFF old").unwrap();
        let path = file(tmp.path(), Recording::Marti).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), MARTI);
        assert!(!tmp.path().join("vapur.wav").exists());
    }
}
