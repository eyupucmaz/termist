//! The local time of day, for the scenes' palettes.

/// The hour (0-23) on the local clock; noon if the clock cannot be read.
pub fn local_hour() -> u32 {
    #[cfg(unix)]
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()) as libc::time_t;
        // SAFETY: localtime_r writes only into the tm it is given.
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&now, &mut tm).is_null() {
                return 12;
            }
            tm.tm_hour.clamp(0, 23) as u32
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
        (time.wHour as u32).min(23)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_hour_is_an_hour() {
        assert!(super::local_hour() < 24);
    }
}
