//! The app's palette: one accent, three state colors, and greys derived from
//! the terminal's own foreground and background.
//!
//! The greys are blends, not fixed values, because a border or a selected-row
//! tint is only as visible as its distance from the background it is drawn
//! on, and the background is the user's. [`probe`] asks the terminal for both
//! colors once at startup; where it cannot, or where the terminal has no
//! 24-bit color, [`Theme::ANSI`] stands in.

use ratatui::style::{Color, Style};
use std::sync::OnceLock;

type Rgb = (u8, u8, u8);

/// The accent and state colors on a dark background.
const ACCENT: Rgb = (86, 212, 221);
const GOOD: Rgb = (86, 211, 100);
const WARN: Rgb = (227, 179, 65);
const BAD: Rgb = (255, 123, 114);

/// The colors every view draws with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Secondary text: headers, units, hints.
    pub dim: Color,
    /// Borders and the empty part of a gauge.
    pub border: Color,
    /// The one accent: the active tab, the focused border, the share bar.
    pub accent: Color,
    /// Text drawn on an accent background.
    pub on_accent: Color,
    /// A gauge with room left, a successful save.
    pub good: Color,
    /// A gauge running high, a setting that waits for a restart.
    pub warn: Color,
    /// A spent gauge, a failure.
    pub bad: Color,
    /// Background of the selected row.
    pub selected_bg: Color,
    /// Foreground of the selected row.
    pub selected_fg: Color,
    /// Background of a row whose numbers just changed.
    pub flash_bg: Color,
}

impl Theme {
    /// The palette for a terminal whose colors are unknown, in the 16 colors
    /// every terminal maps itself.
    pub const ANSI: Self = Self {
        dim: Color::DarkGray,
        border: Color::DarkGray,
        accent: Color::Cyan,
        on_accent: Color::Black,
        good: Color::Green,
        warn: Color::Yellow,
        bad: Color::Red,
        selected_bg: Color::Rgb(40, 52, 66),
        selected_fg: Color::White,
        flash_bg: Color::Rgb(60, 80, 60),
    };

    /// The palette for a terminal drawing `fg` text on a `bg` background.
    pub fn derive(fg: Rgb, bg: Rgb) -> Self {
        // On a light background the pastel accent and state colors wash out,
        // so they are pulled toward the text color.
        let light = luminance(bg) > luminance(fg);
        let tone = |color: Rgb| {
            if light { blend(color, fg, 0.45) } else { color }
        };
        let rgb = |(r, g, b): Rgb| Color::Rgb(r, g, b);
        Self {
            dim: rgb(blend(fg, bg, 0.35)),
            border: rgb(blend(fg, bg, 0.72)),
            accent: rgb(tone(ACCENT)),
            on_accent: rgb(if light { bg } else { (13, 17, 23) }),
            good: rgb(tone(GOOD)),
            warn: rgb(tone(WARN)),
            bad: rgb(tone(BAD)),
            selected_bg: rgb(blend(bg, tone(ACCENT), 0.16)),
            selected_fg: rgb(fg),
            flash_bg: rgb(blend(bg, tone(GOOD), 0.2)),
        }
    }

    /// A block title. A title is drawn on the border and takes its color
    /// unless it names its own.
    pub fn title(self) -> Style {
        Style::default().fg(Color::Reset).bold()
    }

    /// Secondary text.
    pub fn dim(self) -> Style {
        Style::default().fg(self.dim)
    }

    /// A border nothing is focused in.
    pub fn border(self) -> Style {
        Style::default().fg(self.border)
    }

    /// Accent-colored text or border.
    pub fn accent(self) -> Style {
        Style::default().fg(self.accent)
    }

    /// The active tab and the active time range.
    pub fn active(self) -> Style {
        Style::default().fg(self.on_accent).bg(self.accent).bold()
    }

    /// The selected row.
    pub fn selected(self) -> Style {
        Style::default()
            .fg(self.selected_fg)
            .bg(self.selected_bg)
            .bold()
    }

    /// Traffic-light color for a percent consumed.
    pub fn gauge(self, pct: f64) -> Color {
        if pct >= 90.0 {
            self.bad
        } else if pct >= 70.0 {
            self.warn
        } else {
            self.good
        }
    }
}

static THEME: OnceLock<Theme> = OnceLock::new();

/// The palette in use: the probed one once [`probe`] has run, else
/// [`Theme::ANSI`].
pub fn theme() -> Theme {
    THEME.get().copied().unwrap_or(Theme::ANSI)
}

/// Asks the terminal for its default colors and settles the palette on them.
///
/// Call once, in raw mode, before the first input is read: the answer arrives
/// where key presses do. Later calls do nothing.
pub fn probe() {
    THEME.get_or_init(|| {
        let truecolor = std::env::var("COLORTERM")
            .is_ok_and(|value| value.contains("truecolor") || value.contains("24bit"));
        truecolor
            .then(query_default_colors)
            .flatten()
            .map_or(Theme::ANSI, |(fg, bg)| Theme::derive(fg, bg))
    });
}

/// `from` moved `amount` of the way to `to`.
fn blend(from: Rgb, to: Rgb, amount: f32) -> Rgb {
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * amount).round() as u8;
    (mix(from.0, to.0), mix(from.1, to.1), mix(from.2, to.2))
}

fn luminance((r, g, b): Rgb) -> f32 {
    0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b)
}

/// The terminal's default foreground and background, from its replies to OSC
/// 10 and OSC 11.
///
/// A reply this does not read is read later as key presses, so nothing is
/// asked unless stdin is the terminal (with stdin redirected the reply goes to
/// `/dev/tty`, out of reach here), and the wait ends on the reply to a
/// device-attributes request sent last, which every terminal answers. The
/// deadline only bounds a terminal that answers nothing at all.
#[cfg(unix)]
fn query_default_colors() -> Option<(Rgb, Rgb)> {
    use std::io::{IsTerminal, Write};
    use std::time::{Duration, Instant};

    if !std::io::stdin().is_terminal() {
        return None;
    }
    let mut stdout = std::io::stdout();
    stdout
        .write_all(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[c")
        .ok()?;
    stdout.flush().ok()?;

    let deadline = Instant::now() + Duration::from_secs(1);
    let mut reply = Vec::new();
    while !reply.ends_with(b"c") || !reply.windows(3).any(|bytes| bytes == b"\x1b[?") {
        let left = deadline.saturating_duration_since(Instant::now());
        let mut fd = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `fd` is one valid pollfd, and the count passed is 1.
        if unsafe { libc::poll(&raw mut fd, 1, left.as_millis() as libc::c_int) } <= 0 {
            break;
        }
        let mut chunk = [0u8; 256];
        // SAFETY: `chunk` is writable for the `chunk.len()` bytes asked for.
        let read =
            unsafe { libc::read(libc::STDIN_FILENO, chunk.as_mut_ptr().cast(), chunk.len()) };
        if read <= 0 {
            break;
        }
        reply.extend_from_slice(&chunk[..read as usize]);
    }
    let reply = String::from_utf8_lossy(&reply);
    Some((
        parse_color_reply(&reply, 10)?,
        parse_color_reply(&reply, 11)?,
    ))
}

#[cfg(not(unix))]
fn query_default_colors() -> Option<(Rgb, Rgb)> {
    None
}

/// Reads `rgb:RRRR/GGGG/BBBB` out of the reply to OSC `code`; each channel is
/// one to four hex digits.
#[cfg_attr(not(unix), allow(dead_code))]
fn parse_color_reply(reply: &str, code: u8) -> Option<Rgb> {
    let start = reply.find(&format!("\x1b]{code};rgb:"))?;
    let body = &reply[start..].split_once("rgb:")?.1;
    let end = body.find(['\x1b', '\x07']).unwrap_or(body.len());
    let mut channels = body[..end].split('/').map(|hex| {
        let value = u32::from_str_radix(hex, 16).ok()?;
        let max = (1u32 << (4 * hex.len() as u32)) - 1;
        (hex.len() <= 4 && max > 0).then(|| (value * 255 / max) as u8)
    });
    let color = (channels.next()??, channels.next()??, channels.next()??);
    channels.next().is_none().then_some(color)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_replies_parse_at_every_channel_width() {
        let reply = "\x1b]10;rgb:cdcd/d6d6/f4f4\x1b\\\x1b]11;rgb:1e1e/1e1e/2e2e\x07\x1b[?62;4c";
        assert_eq!(parse_color_reply(reply, 10), Some((0xcd, 0xd6, 0xf4)));
        assert_eq!(parse_color_reply(reply, 11), Some((0x1e, 0x1e, 0x2e)));
        assert_eq!(
            parse_color_reply("\x1b]11;rgb:f/8/0\x07", 11),
            Some((255, 136, 0))
        );
        assert_eq!(
            parse_color_reply("\x1b]11;rgb:ff/80/00\x07", 11),
            Some((255, 128, 0))
        );
        // A terminal that answered only the device-attributes request.
        assert_eq!(parse_color_reply("\x1b[?62;4c", 11), None);
        assert_eq!(parse_color_reply("\x1b]11;rgb:zz/00/00\x07", 11), None);
        assert_eq!(parse_color_reply("\x1b]11;rgb:00/00\x07", 11), None);
    }

    #[test]
    fn greys_sit_between_the_text_and_the_background() {
        let (fg, bg) = ((205, 214, 244), (30, 30, 46));
        let theme = Theme::derive(fg, bg);
        let level = |color: Color| match color {
            Color::Rgb(r, g, b) => luminance((r, g, b)),
            other => panic!("not an rgb color: {other:?}"),
        };
        // Text, secondary text, border, selected row, background: each step
        // is further from the text and still off the background.
        assert!(luminance(fg) > level(theme.dim));
        assert!(level(theme.dim) > level(theme.border));
        assert!(level(theme.border) > level(theme.selected_bg));
        assert!(level(theme.selected_bg) > luminance(bg));
        assert_eq!(theme.accent, Color::Rgb(86, 212, 221));
    }

    #[test]
    fn a_light_background_darkens_the_accent_and_state_colors() {
        let dark = Theme::derive((205, 214, 244), (30, 30, 46));
        let light = Theme::derive((36, 41, 47), (255, 255, 255));
        let level = |color: Color| match color {
            Color::Rgb(r, g, b) => luminance((r, g, b)),
            other => panic!("not an rgb color: {other:?}"),
        };
        for (on_light, on_dark) in [
            (light.accent, dark.accent),
            (light.good, dark.good),
            (light.warn, dark.warn),
            (light.bad, dark.bad),
        ] {
            assert!(level(on_light) < level(on_dark));
        }
        // The greys still fall between the two.
        assert!(level(light.border) > level(light.dim));
    }
}
