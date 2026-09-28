//! Asking the host terminal for its default colours (OSC 10 and 11), so agents drawn in
//! the terminal's own colours can be told what those are.
use std::time::Duration;

pub type Rgb = (u8, u8, u8);

/// The host terminal's default foreground and background, if it says within `wait`.
/// Call it in raw mode, before anything else reads the terminal: the answers arrive as
/// input. A device-attributes query goes last, because every terminal answers that
/// one; its answer ends the wait early.
pub fn query(wait: Duration) -> Option<(Rgb, Rgb)> {
    #[cfg(unix)]
    {
        let tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .ok()?;
        unix::query(&tty, wait)
    }
    #[cfg(windows)]
    {
        let _ = wait;
        None
    }
}

#[cfg_attr(not(unix), allow(dead_code))] // Windows asks nothing yet
const QUERY: &[u8] = b"\x1b]10;?\x07\x1b]11;?\x07\x1b[c";

/// The colours in `answer` (the bytes a terminal sent back to [`QUERY`]).
pub fn parse(answer: &[u8]) -> Option<(Rgb, Rgb)> {
    let text = String::from_utf8_lossy(answer);
    Some((colour(&text, "10")?, colour(&text, "11")?))
}

/// Whether `answer` holds the device-attributes reply, `ESC [ ? … c`.
#[cfg_attr(not(unix), allow(dead_code))]
fn has_device_attributes(answer: &[u8]) -> bool {
    answer.windows(3).enumerate().any(|(i, w)| {
        w == b"\x1b[?"
            && answer[i + 3..]
                .iter()
                .find(|b| !(b.is_ascii_digit() || **b == b';'))
                == Some(&b'c')
    })
}

/// `ESC ] <n> ; rgb:R/G/B` ended by BEL or ST, each part 1 to 4 hex digits.
fn colour(text: &str, n: &str) -> Option<Rgb> {
    let start = text.find(&format!("\x1b]{n};rgb:"))? + n.len() + 7;
    let body: String = text[start..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit() || *c == '/')
        .collect();
    let mut parts = body.split('/').map(|p| {
        let v = u32::from_str_radix(p, 16).ok()?;
        let max = 16u32.checked_pow(p.len() as u32)?.checked_sub(1)?;
        (1..=4)
            .contains(&p.len())
            .then(|| ((v * 255 + max / 2) / max) as u8)
    });
    let rgb = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(rgb)
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::time::Instant;

    pub fn query(tty: &std::fs::File, wait: Duration) -> Option<(Rgb, Rgb)> {
        let fd = tty.as_raw_fd();
        if fd >= libc::FD_SETSIZE as i32 {
            return None; // select cannot watch it
        }
        let mut out = tty;
        out.write_all(QUERY).ok()?;
        out.flush().ok()?;
        let deadline = Instant::now() + wait;
        let mut answer = Vec::new();
        let mut buf = [0u8; 256];
        while !has_device_attributes(&answer) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            // select, not poll: macOS's poll does not work on terminal devices.
            let ready = unsafe {
                // SAFETY: a zeroed fd_set is empty; fd is open and below FD_SETSIZE.
                let mut read_set: libc::fd_set = std::mem::zeroed();
                libc::FD_ZERO(&mut read_set);
                libc::FD_SET(fd, &mut read_set);
                let mut timeout = libc::timeval {
                    tv_sec: left.as_secs() as _,
                    tv_usec: left.subsec_micros() as _,
                };
                libc::select(
                    fd + 1,
                    &mut read_set,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut timeout,
                )
            };
            if ready <= 0 {
                break;
            }
            let mut input = tty;
            match input.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => answer.extend_from_slice(&buf[..n]),
            }
        }
        parse(&answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_and_two_digit_answers_ended_by_bel_or_st() {
        let answer = b"\x1b]10;rgb:e6e6/eded/f3f3\x07\x1b]11;rgb:0f/1d/2e\x1b\\\x1b[?62;22c";
        assert_eq!(
            parse(answer),
            Some(((0xe6, 0xed, 0xf3), (0x0f, 0x1d, 0x2e)))
        );
        assert!(has_device_attributes(answer));
    }

    #[test]
    fn one_and_three_digit_parts_are_scaled() {
        let answer = b"\x1b]10;rgb:f/8/0\x07\x1b]11;rgb:fff/800/000\x07";
        assert_eq!(parse(answer), Some(((255, 136, 0), (255, 128, 0))));
    }

    #[test]
    fn a_terminal_that_only_knows_device_attributes_gives_nothing() {
        let answer = b"\x1b[?1;2c";
        assert_eq!(parse(answer), None);
        assert!(has_device_attributes(answer));
        assert!(!has_device_attributes(b"\x1b[?1;2"));
    }

    #[test]
    fn broken_answers_give_nothing() {
        for answer in [
            &b"\x1b]10;rgb:ff/ff\x07\x1b]11;rgb:00/00/00\x07"[..],
            b"\x1b]10;rgb:gg/ff/ff\x07\x1b]11;rgb:00/00/00\x07",
            b"\x1b]10;rgb:fffff/0/0\x07\x1b]11;rgb:0/0/0\x07",
            b"\x1b]11;rgb:00/00/00\x07",
        ] {
            assert_eq!(parse(answer), None, "{:?}", String::from_utf8_lossy(answer));
        }
    }

    /// A pseudo-terminal stands in for the host: it answers, or it stays silent and the
    /// query gives up at its deadline.
    #[cfg(unix)]
    #[test]
    fn a_terminal_answers_or_the_query_gives_up_in_time() {
        use std::io::{Read, Write};
        use std::os::fd::FromRawFd;
        use std::time::Instant;
        for answers in [true, false] {
            let (mut master, slave) = unsafe {
                let (mut m, mut s) = (0, 0);
                assert_eq!(
                    libc::openpty(
                        &mut m,
                        &mut s,
                        std::ptr::null_mut(),
                        // *mut on macOS, *const on Linux: a null *mut suits both.
                        std::ptr::null_mut(),
                        std::ptr::null_mut()
                    ),
                    0
                );
                let mut t: libc::termios = std::mem::zeroed();
                libc::tcgetattr(s, &mut t);
                libc::cfmakeraw(&mut t);
                libc::tcsetattr(s, libc::TCSANOW, &t);
                (std::fs::File::from_raw_fd(m), std::fs::File::from_raw_fd(s))
            };
            let host = std::thread::spawn(move || {
                let mut asked = vec![0u8; QUERY.len()];
                master.read_exact(&mut asked).unwrap();
                assert_eq!(asked, QUERY);
                if answers {
                    master
                        .write_all(b"\x1b]10;rgb:2b2b/2525/3030\x1b\\\x1b]11;rgb:fbfb/f4f4/e8e8\x1b\\\x1b[?62c")
                        .unwrap();
                }
                master // closing it early would end the read with an error
            });
            let started = Instant::now();
            let got = unix::query(&slave, Duration::from_millis(300));
            let took = started.elapsed();
            let _master = host.join().unwrap();
            if answers {
                assert_eq!(got, Some(((0x2b, 0x25, 0x30), (0xfb, 0xf4, 0xe8))));
                assert!(
                    took < Duration::from_millis(250),
                    "{took:?}: the DA answer ends the wait"
                );
            } else {
                assert_eq!(got, None);
                assert!(took < Duration::from_millis(600), "{took:?}");
            }
        }
    }
}
