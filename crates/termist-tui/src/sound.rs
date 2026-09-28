//! termist's own sound: a seagull (martı) when an agent waits for you or is done.
//! The recording is built into the binary as a WAV file, so every platform's player
//! can play it.
use std::path::{Path, PathBuf};

/// The martı: mono, 16-bit, 44.1 kHz.
pub const MARTI: &[u8] = include_bytes!("../../../assets/sounds/marti.wav");

/// The sound as a WAV file in `dir`, written the first time it is needed.
pub fn file(dir: &Path) -> std::io::Result<PathBuf> {
    let path = dir.join("marti.wav");
    let current = std::fs::read(&path).is_ok_and(|b| b == MARTI);
    if !current {
        std::fs::create_dir_all(dir)?;
        std::fs::write(&path, MARTI)?;
        // Earlier releases also wrote a ferry's horn here.
        let _ = std::fs::remove_file(dir.join("vapur.wav"));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marti_is_a_mono_16_bit_wav() {
        assert_eq!(&MARTI[..4], b"RIFF");
        assert_eq!(&MARTI[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes([MARTI[20], MARTI[21]]), 1, "PCM");
        assert_eq!(u16::from_le_bytes([MARTI[22], MARTI[23]]), 1, "mono");
        assert_eq!(
            u32::from_le_bytes(MARTI[24..28].try_into().unwrap()),
            44_100
        );
        assert_eq!(u16::from_le_bytes([MARTI[34], MARTI[35]]), 16);
    }

    #[test]
    fn files_are_written_once() {
        let tmp = tempfile::tempdir().unwrap();
        let path = file(tmp.path()).unwrap();
        let written = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(file(tmp.path()).unwrap(), path);
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
        let path = file(tmp.path()).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), MARTI);
        assert!(!tmp.path().join("vapur.wav").exists());
    }
}
