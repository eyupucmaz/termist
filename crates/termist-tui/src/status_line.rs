//! The status line on the top right: `cpu 23%  ram 11.2/16G  ⚡84%  14:32`.
use crate::theme::Theme;
use ratatui::text::Span;
use termist_core::config::StatusConfig;
use termist_platform::sysstat::SysStat;

const GIB: f64 = (1u64 << 30) as f64;

/// The parts that are on and read, cut from the left (cpu first, the clock last) to fit
/// `width` cells with a space at the end; nothing if not even the clock fits.
pub fn spans(
    stat: &SysStat,
    cfg: &StatusConfig,
    time: (u32, u32),
    theme: &Theme,
    width: usize,
) -> Vec<Span<'static>> {
    let mut parts: Vec<Span<'static>> = Vec::new();
    if let (true, Some(cpu)) = (cfg.cpu, stat.cpu) {
        let style = if cpu > 85.0 { theme.warn } else { theme.dim };
        parts.push(Span::styled(format!("cpu {cpu:.0}%"), style));
    }
    if let (true, Some((used, total))) = (cfg.ram, stat.ram) {
        let style = if used as f64 > total as f64 * 0.9 {
            theme.warn
        } else {
            theme.dim
        };
        let text = format!(
            "ram {:.1}/{:.0}G",
            used as f64 / GIB,
            (total as f64 / GIB).round()
        );
        parts.push(Span::styled(text, style));
    }
    if let (true, Some(b)) = (cfg.battery, stat.battery) {
        let (text, style) = if b.charging {
            (format!("⚡{}%", b.percent), theme.dim)
        } else if b.percent < 20 {
            (format!("bat {}%", b.percent), theme.error)
        } else {
            (format!("bat {}%", b.percent), theme.dim)
        };
        parts.push(Span::styled(text, style));
    }
    if cfg.clock {
        parts.push(Span::styled(
            format!("{:02}:{:02}", time.0, time.1),
            theme.dim,
        ));
    }
    let cells = |parts: &[Span]| -> usize {
        parts.iter().map(Span::width).sum::<usize>() + 2 * parts.len().saturating_sub(1) + 1
    };
    while !parts.is_empty() && cells(&parts) > width {
        parts.remove(0);
    }
    let mut out = Vec::new();
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            out.push(Span::raw("  "));
        }
        out.push(part);
    }
    if !out.is_empty() {
        out.push(Span::raw(" "));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use termist_platform::sysstat::Battery;

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn stat() -> SysStat {
        SysStat {
            cpu: Some(23.4),
            ram: Some(((11.2 * GIB) as u64, 16 * (1 << 30))),
            battery: Some(Battery {
                percent: 84,
                charging: true,
            }),
        }
    }

    #[test]
    fn everything_fits_on_a_wide_screen() {
        let t = Theme::terminal();
        let s = spans(&stat(), &StatusConfig::default(), (14, 32), &t, 200);
        assert_eq!(text(&s), "cpu 23%  ram 11.2/16G  ⚡84%  14:32 ");
    }

    #[test]
    fn parts_go_from_the_left_as_the_room_shrinks() {
        let t = Theme::terminal();
        let c = StatusConfig::default();
        let full = text(&spans(&stat(), &c, (14, 32), &t, 200));
        let w = full.chars().count();
        assert_eq!(
            text(&spans(&stat(), &c, (14, 32), &t, w - 1)),
            "ram 11.2/16G  ⚡84%  14:32 "
        );
        assert_eq!(text(&spans(&stat(), &c, (14, 32), &t, 14)), "⚡84%  14:32 ");
        assert_eq!(text(&spans(&stat(), &c, (14, 32), &t, 6)), "14:32 ");
        assert!(spans(&stat(), &c, (14, 32), &t, 5).is_empty());
    }

    #[test]
    fn the_status_fits_any_width() {
        let t = Theme::terminal();
        for w in 0..60 {
            let s = spans(&stat(), &StatusConfig::default(), (9, 5), &t, w);
            assert!(text(&s).chars().count() <= w, "{w}");
        }
    }

    #[test]
    fn parts_that_are_off_or_unread_do_not_show() {
        let t = Theme::terminal();
        let cfg = StatusConfig {
            cpu: false,
            ..StatusConfig::default()
        };
        assert_eq!(
            text(&spans(&stat(), &cfg, (9, 5), &t, 200)),
            "ram 11.2/16G  ⚡84%  09:05 "
        );
        let nothing_yet = SysStat::default();
        assert_eq!(
            text(&spans(
                &nothing_yet,
                &StatusConfig::default(),
                (9, 5),
                &t,
                200
            )),
            "09:05 "
        );
    }

    #[test]
    fn a_battery_off_power_says_bat() {
        let t = Theme::terminal();
        let mut s = stat();
        s.battery = Some(Battery {
            percent: 50,
            charging: false,
        });
        let cfg = StatusConfig {
            cpu: false,
            ram: false,
            clock: false,
            ..StatusConfig::default()
        };
        assert_eq!(text(&spans(&s, &cfg, (0, 0), &t, 200)), "bat 50% ");
    }

    #[test]
    fn busy_and_low_take_the_warning_colours() {
        let t = Theme::named("uskudar", termist_core::config::ColorDepth::TrueColor);
        let busy = SysStat {
            cpu: Some(90.0),
            ram: Some((95, 100)),
            battery: Some(Battery {
                percent: 15,
                charging: false,
            }),
        };
        let s = spans(&busy, &StatusConfig::default(), (0, 0), &t, 200);
        let style_of = |prefix: &str| {
            s.iter()
                .find(|x| x.content.starts_with(prefix))
                .unwrap()
                .style
        };
        assert_eq!(style_of("cpu"), t.warn);
        assert_eq!(style_of("ram"), t.warn);
        assert_eq!(style_of("bat"), t.error);
        assert_eq!(style_of("00:00"), t.dim);
    }
}
