use std::time::Duration;

pub fn size(bytes: usize) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn duration(elapsed: Duration) -> String {
    let millis = elapsed.as_millis();
    if millis < 1000 {
        format!("{millis} ms")
    } else {
        format!("{:.2} s", elapsed.as_secs_f64())
    }
}

pub fn ago(seconds: i64) -> String {
    match seconds {
        ..60 => "just now".into(),
        60..3600 => format!("{} min ago", seconds / 60),
        3600..86_400 => format!("{} h ago", seconds / 3600),
        86_400..172_800 => "yesterday".into(),
        _ => format!("{} days ago", seconds / 86_400),
    }
}

/// How long until something expires, `seconds` from now.
pub fn until(seconds: i64) -> String {
    match seconds {
        ..60 => "in a minute".into(),
        60..3600 => format!("in {} min", seconds / 60),
        3600..86_400 => format!("in {} h", seconds / 3600),
        _ => format!("in {} days", seconds / 86_400),
    }
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

/// On one line, cut after `max` characters.
pub fn truncate(text: &str, max: usize) -> String {
    let line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(max) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_use_binary_units() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1023), "1023 B");
        assert_eq!(size(1536), "1.5 KB");
        assert_eq!(size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn durations_switch_to_seconds() {
        assert_eq!(duration(Duration::from_millis(42)), "42 ms");
        assert_eq!(duration(Duration::from_millis(1500)), "1.50 s");
    }

    #[test]
    fn ages_are_relative() {
        assert_eq!(ago(-5), "just now");
        assert_eq!(ago(59), "just now");
        assert_eq!(ago(125), "2 min ago");
        assert_eq!(ago(7200), "2 h ago");
        assert_eq!(ago(90_000), "yesterday");
        assert_eq!(ago(3 * 86_400), "3 days ago");
    }

    #[test]
    fn expiries_are_relative() {
        assert_eq!(until(5), "in a minute");
        assert_eq!(until(600), "in 10 min");
        assert_eq!(until(7200), "in 2 h");
        assert_eq!(until(3 * 86_400), "in 3 days");
    }

    #[test]
    fn truncate_keeps_one_line() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a\n  b", 10), "a b");
        assert_eq!(truncate("éééé", 2), "éé…");
    }
}
