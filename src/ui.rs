//! Human terminal rendering. Used only when stdout is a terminal and no
//! `--format` was requested; agents, pipes and MCP get the structured output.

use std::io::IsTerminal as _;

use chrono::{DateTime, Local, Utc};

pub const MPH_PER_MPS: f64 = 2.236_936_3;
pub const YD_PER_M: f64 = 1.093_613_3;

fn color_enabled(stream_is_terminal: bool) -> bool {
    stream_is_terminal && std::env::var_os("NO_COLOR").is_none()
}

fn paint(code: &str, text: &str, on: bool) -> String {
    if on { format!("\x1b[{code}m{text}\x1b[0m") } else { text.to_owned() }
}

/// Styling for stdout.
pub fn bold(text: &str) -> String {
    paint("1", text, color_enabled(std::io::stdout().is_terminal()))
}
pub fn dim(text: &str) -> String {
    paint("2", text, color_enabled(std::io::stdout().is_terminal()))
}
pub fn green(text: &str) -> String {
    paint("32", text, color_enabled(std::io::stdout().is_terminal()))
}
pub fn yellow(text: &str) -> String {
    paint("33", text, color_enabled(std::io::stdout().is_terminal()))
}

/// Styling for stderr (live progress and prompts).
pub fn err_bold(text: &str) -> String {
    paint("1", text, color_enabled(std::io::stderr().is_terminal()))
}
pub fn err_dim(text: &str) -> String {
    paint("2", text, color_enabled(std::io::stderr().is_terminal()))
}
pub fn err_yellow(text: &str) -> String {
    paint("33", text, color_enabled(std::io::stderr().is_terminal()))
}

pub enum Align {
    Left,
    Right,
}

/// A plain column-aligned table with a bold header row. Cells must not contain ANSI codes.
pub fn table(headers: &[(&str, Align)], rows: &[Vec<String>]) -> String {
    let widths: Vec<usize> = headers
        .iter()
        .enumerate()
        .map(|(i, (h, _))| {
            rows.iter()
                .map(|r| r.get(i).map_or(0, |c| c.chars().count()))
                .max()
                .unwrap_or(0)
                .max(h.chars().count())
        })
        .collect();
    let line = |cells: Vec<&str>| -> String {
        let mut out = String::new();
        for (i, cell) in cells.iter().enumerate() {
            let pad = widths[i].saturating_sub(cell.chars().count());
            let last = i + 1 == cells.len();
            match headers[i].1 {
                Align::Right => {
                    out.push_str(&" ".repeat(pad));
                    out.push_str(cell);
                }
                Align::Left => {
                    out.push_str(cell);
                    if !last {
                        out.push_str(&" ".repeat(pad));
                    }
                }
            }
            if !last {
                out.push_str("  ");
            }
        }
        out
    };
    let mut out = bold(&line(headers.iter().map(|(h, _)| *h).collect()));
    for row in rows {
        out.push('\n');
        out.push_str(&line(row.iter().map(String::as_str).collect()));
    }
    out
}

/// Aligned `label  value` lines with dim labels.
pub fn fields(rows: &[(&str, String)]) -> String {
    let width = rows.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0);
    rows.iter()
        .map(|(k, v)| format!("{}  {v}", dim(&format!("{k:<width$}"))))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `Sat 13 May 2023 17:24` in the local time zone.
pub fn local_time(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%a %e %b %Y %H:%M").to_string().replace("  ", " ")
}

/// `in 13 days`, `5 minutes ago`.
pub fn relative(t: DateTime<Utc>) -> String {
    let delta = t - Utc::now();
    let (n, unit) = {
        let s = delta.num_seconds().abs();
        match s {
            0..=89 => return if delta.num_seconds() >= 0 { "in a moment".into() } else { "just now".into() },
            90..=5399 => ((s + 30) / 60, "minute"),
            5400..=129_599 => ((s + 1800) / 3600, "hour"),
            _ => ((s + 43_200) / 86_400, "day"),
        }
    };
    let unit = if n == 1 { unit.to_owned() } else { format!("{unit}s") };
    if delta.num_seconds() >= 0 { format!("in {n} {unit}") } else { format!("{n} {unit} ago") }
}

/// `SHOT_ANALYSIS` → `Shot analysis`.
pub fn kind_label(kind: &str) -> String {
    let lower = kind.replace('_', " ").to_lowercase();
    let mut chars = lower.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Seconds as `3d 4h`, `5h 12m`, `42s`.
pub fn duration(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {}s", s / 60, s % 60),
        3600..=86_399 => format!("{}h {}m", s / 3600, (s % 3600) / 60),
        _ => format!("{}d {}h", s / 86_400, (s % 86_400) / 3600),
    }
}

/// File size as `812 B`, `14.2 KB`, `3.1 MB`.
pub fn bytes(n: u64) -> String {
    match n {
        0..=1023 => format!("{n} B"),
        1024..=1_048_575 => format!("{:.1} KB", n as f64 / 1024.0),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

/// Signed seconds with an explicit sign, chosen precision by magnitude.
pub fn offset(seconds: f64) -> String {
    if seconds.abs() < 1.0 {
        format!("{:+.1} ms", seconds * 1000.0)
    } else {
        format!("{seconds:+.3} s")
    }
}

/// `TM4 clock is 1.500 s ahead of this computer (±0.1 ms)`. `dim` picks the
/// stream styling (stdout or stderr).
pub fn clock_sentence(offset_s: f64, delay_s: f64, dim: fn(&str) -> String) -> String {
    let direction = if offset_s >= 0.0 { "ahead of" } else { "behind" };
    format!(
        "TM4 clock is {} {direction} this computer {}",
        offset(offset_s.abs()).trim_start_matches('+'),
        dim(&format!("(±{})", offset(delay_s / 2.0).trim_start_matches('+')))
    )
}

/// TrackMan club codes to words: `6Iron` → `6 Iron`, `PitchingWedge` →
/// `Pitching Wedge`, `56Wedge` → `56 Wedge`.
pub fn club_label(club: &str) -> String {
    let mut out = String::with_capacity(club.len() + 4);
    let mut prev: Option<char> = None;
    for c in club.chars() {
        if let Some(p) = prev {
            let boundary = (p.is_ascii_digit() && c.is_alphabetic())
                || (p.is_lowercase() && (c.is_uppercase() || c.is_ascii_digit()));
            if boundary {
                out.push(' ');
            }
        }
        out.push(c);
        prev = Some(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::club_label;

    #[test]
    fn club_label_splits_trackman_codes() {
        for (raw, label) in [
            ("6Iron", "6 Iron"),
            ("56Wedge", "56 Wedge"),
            ("PitchingWedge", "Pitching Wedge"),
            ("3Wood", "3 Wood"),
            ("Hybrid4", "Hybrid 4"),
            ("Driver", "Driver"),
            ("7 Iron", "7 Iron"),
        ] {
            assert_eq!(club_label(raw), label, "{raw}");
        }
    }
}
