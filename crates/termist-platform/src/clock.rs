//! The local time of day, for the scenes' palettes and the status line.

/// The hour (0-23) and minute on the local clock; noon if the clock cannot be read.
pub fn local_time() -> (u32, u32) {
    #[cfg(unix)]
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()) as libc::time_t;
        // SAFETY: localtime_r writes only into the tm it is given.
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&now, &mut tm).is_null() {
                return (12, 0);
            }
            (
                tm.tm_hour.clamp(0, 23) as u32,
                tm.tm_min.clamp(0, 59) as u32,
            )
        }
    }
    #[cfg(windows)]
    {
        // SAFETY: GetLocalTime fills the SYSTEMTIME it is given.
        let time = unsafe {
            let mut time: windows_sys::Win32::Foundation::SYSTEMTIME = std::mem::zeroed();
            windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut time);
            time
        };
        ((time.wHour as u32).min(23), (time.wMinute as u32).min(59))
    }
}

/// The hour (0-23) on the local clock; noon if the clock cannot be read.
pub fn local_hour() -> u32 {
    local_time().0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_local_time_is_a_time_of_day() {
        let (h, m) = local_time();
        assert!(h < 24 && m < 60, "{h}:{m}");
    }

    #[test]
    fn the_hour_is_an_hour() {
        assert!(super::local_hour() < 24);
    }
}
