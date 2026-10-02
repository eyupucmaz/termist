//! What the machine is doing, for the status line: CPU, memory and battery.
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SysStat {
    /// Percent of all cores, since the reading before.
    pub cpu: Option<f32>,
    /// Bytes used and in all.
    pub ram: Option<(u64, u64)>,
    pub battery: Option<Battery>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Battery {
    pub percent: u8,
    /// On power: charging or full.
    pub charging: bool,
}

/// Reads the machine. CPU use is the change since the reading before, so the first
/// `cpu_ram` should come at least `sysinfo::MINIMUM_CPU_UPDATE_INTERVAL` after `new`.
pub struct Sampler {
    sys: System,
    batteries: Option<starship_battery::Manager>,
}

impl Sampler {
    pub fn new() -> Sampler {
        let sys = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
                .with_memory(MemoryRefreshKind::nothing().with_ram()),
        );
        Sampler {
            sys,
            batteries: starship_battery::Manager::new().ok(),
        }
    }

    pub fn cpu_ram(&mut self) -> (Option<f32>, Option<(u64, u64)>) {
        self.sys.refresh_cpu_usage();
        self.sys
            .refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
        let cpu = self.sys.global_cpu_usage().clamp(0.0, 100.0);
        let total = self.sys.total_memory();
        let ram = (total > 0).then(|| (self.sys.used_memory().min(total), total));
        (Some(cpu), ram)
    }

    /// The first battery, or `None` on a machine without one.
    pub fn battery(&mut self) -> Option<Battery> {
        let battery = self.batteries.as_ref()?.batteries().ok()?.next()?.ok()?;
        let percent = (battery.state_of_charge().value * 100.0)
            .round()
            .clamp(0.0, 100.0) as u8;
        let charging = matches!(
            battery.state(),
            starship_battery::State::Charging | starship_battery::State::Full
        );
        Some(Battery { percent, charging })
    }
}

impl Default for Sampler {
    fn default() -> Self {
        Sampler::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_machine_has_memory_and_a_cpu_reading() {
        let mut s = Sampler::new();
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        let (cpu, ram) = s.cpu_ram();
        let cpu = cpu.expect("a cpu reading");
        assert!((0.0..=100.0).contains(&cpu), "{cpu}");
        let (used, total) = ram.expect("a memory reading");
        assert!(total > 0 && used <= total, "{used}/{total}");
    }

    #[test]
    fn a_battery_is_a_percentage_or_none() {
        if let Some(b) = Sampler::new().battery() {
            assert!(b.percent <= 100, "{b:?}");
        }
    }
}
