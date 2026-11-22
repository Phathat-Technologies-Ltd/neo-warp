//! Converts a vt100 screen into lines of styled runs for rendering a recording with its colors.

use pathfinder_color::ColorU;

/// Colors used to resolve the screen's default and indexed colors.
#[derive(Clone, Debug)]
pub struct Palette {
    pub foreground: ColorU,
    pub background: ColorU,
    /// The 16 ANSI colors: 8 normal, then 8 bright.
    pub ansi: [ColorU; 16],
}

#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub text: String,
    pub foreground: ColorU,
    /// `None` when the cell uses the default background.
    pub background: Option<ColorU>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

/// One run list per screen row. Trailing default-styled blanks are trimmed.
pub fn styled_lines(screen: &vt100::Screen, palette: &Palette) -> Vec<Vec<Run>> {
    let (rows, cols) = screen.size();
    (0..rows)
        .map(|row| {
            let mut runs: Vec<Run> = Vec::new();
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let mut foreground = resolve(cell.fgcolor(), palette, true, cell.bold());
                let mut background = match cell.bgcolor() {
                    vt100::Color::Default => None,
                    color => Some(resolve(color, palette, false, false)),
                };
                if cell.inverse() {
                    let new_background = foreground;
                    foreground = background.unwrap_or(palette.background);
                    background = Some(new_background);
                }
                let contents = if cell.has_contents() {
                    cell.contents()
                } else {
                    " "
                };
                match runs.last_mut() {
                    Some(run)
                        if run.foreground == foreground
                            && run.background == background
                            && run.bold == cell.bold()
                            && run.italic == cell.italic()
                            && run.underline == cell.underline() =>
                    {
                        run.text.push_str(contents);
                    }
                    _ => runs.push(Run {
                        text: contents.to_owned(),
                        foreground,
                        background,
                        bold: cell.bold(),
                        italic: cell.italic(),
                        underline: cell.underline(),
                    }),
                }
            }
            trim_trailing_blanks(&mut runs);
            runs
        })
        .collect()
}

fn trim_trailing_blanks(runs: &mut Vec<Run>) {
    while let Some(run) = runs.last_mut() {
        if run.background.is_some() || run.underline {
            break;
        }
        let trimmed_len = run.text.trim_end_matches(' ').len();
        run.text.truncate(trimmed_len);
        if run.text.is_empty() {
            runs.pop();
        } else {
            break;
        }
    }
}

fn resolve(color: vt100::Color, palette: &Palette, is_foreground: bool, bold: bool) -> ColorU {
    match color {
        vt100::Color::Default if is_foreground => palette.foreground,
        vt100::Color::Default => palette.background,
        // Bold text in a normal ANSI color is drawn bright, like most terminals.
        vt100::Color::Idx(index) if index < 8 && bold && is_foreground => {
            palette.ansi[usize::from(index) + 8]
        }
        vt100::Color::Idx(index) => indexed(index, palette),
        vt100::Color::Rgb(r, g, b) => ColorU::new(r, g, b, 255),
    }
}

/// The xterm 256-color palette, with the first 16 taken from the theme.
fn indexed(index: u8, palette: &Palette) -> ColorU {
    match index {
        0..=15 => palette.ansi[usize::from(index)],
        16..=231 => {
            let index = index - 16;
            let level = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            ColorU::new(
                level(index / 36),
                level((index / 6) % 6),
                level(index % 6),
                255,
            )
        }
        232..=255 => {
            let gray = 8 + (index - 232) * 10;
            ColorU::new(gray, gray, gray, 255)
        }
    }
}

#[cfg(test)]
#[path = "screen_style_tests.rs"]
mod tests;
