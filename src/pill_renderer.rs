use tiny_skia::{Color, FillRule, Paint, Path, PathBuilder, Pixmap, Transform};

pub const WIDTH: f32 = 285.0;
pub const HEIGHT: f32 = 64.0;
const BORDER_WIDTH: f32 = 2.0;
/// Keep the antialiased edge inside the layered bitmap.
const PILL_EDGE_INSET: f32 = 1.0;
/// Horizontal margin between the pill edge and the bar area. Identical on
/// both ends by construction.
const BAR_MARGIN: f32 = 12.0;
const BAR_VERTICAL_INSET: f32 = 8.0;
const BAR_MIN_HEIGHT: f32 = 3.0;
const IDLE_BAR_VALUE: f32 = 0.06;
const BACKGROUND: [u8; 4] = [6, 19, 25, 255];
const LISTENING_BORDER: [u8; 4] = [0, 197, 224, 255];
const WORKING_BORDER: [u8; 4] = [0, 101, 115, 255];
const ERROR_BORDER: [u8; 4] = [217, 69, 61, 255];
const _: () = assert!(HEIGHT <= 96.0, "dictate overlay stays pill height");
const _: () = assert!(WIDTH <= 480.0, "dictate overlay stays compact");
const _: () = assert!(HEIGHT < WIDTH, "pill stays wider than tall");

/// Visual lifecycle of the dictate pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PillState {
    /// Microphone hot. Bars follow the live input level.
    Listening,
    /// Transcribing or inserting. Static status text, distinct from listening.
    Working,
    /// Last operation failed.
    Error,
    /// Text actions and other quiet states.
    Idle,
}

impl PillState {
    fn border(self) -> [u8; 4] {
        match self {
            Self::Listening => LISTENING_BORDER,
            Self::Working => WORKING_BORDER,
            Self::Error => ERROR_BORDER,
            Self::Idle => BACKGROUND,
        }
    }
}

/// Render one antialiased pill frame as premultiplied RGBA pixels.
///
/// Windows presentation swaps these bytes to premultiplied BGRA before
/// passing the frame to `UpdateLayeredWindow`.
pub fn render_rgba(
    pixel_width: u32,
    pixel_height: u32,
    bars: &[f32],
    state: PillState,
) -> Option<Vec<u8>> {
    let mut pixmap = Pixmap::new(pixel_width, pixel_height)?;
    let transform = Transform::from_scale(pixel_width as f32 / WIDTH, pixel_height as f32 / HEIGHT);

    fill_capsule(
        &mut pixmap,
        capsule_path(
            PILL_EDGE_INSET,
            PILL_EDGE_INSET,
            WIDTH - 2.0 * PILL_EDGE_INSET,
            HEIGHT - 2.0 * PILL_EDGE_INSET,
        )?,
        state.border(),
        transform,
    );

    let inner_inset = PILL_EDGE_INSET + BORDER_WIDTH;
    fill_capsule(
        &mut pixmap,
        capsule_path(
            inner_inset,
            inner_inset,
            WIDTH - 2.0 * inner_inset,
            HEIGHT - 2.0 * inner_inset,
        )?,
        BACKGROUND,
        transform,
    );
    symmetrize_horizontal(&mut pixmap);
    symmetrize_vertical(&mut pixmap);

    if state != PillState::Working {
        let (bars_x, bars_width) = bars_area(WIDTH);
        let count = bars.len().max(1);
        let specs = bar_specs(bars, bars_width, count);
        let bars_height = HEIGHT - 2.0 * BAR_VERTICAL_INSET;
        for spec in &specs {
            let value = match state {
                PillState::Listening | PillState::Error => spec.value,
                PillState::Working | PillState::Idle => IDLE_BAR_VALUE,
            };
            let height = BAR_MIN_HEIGHT + value * (bars_height - BAR_MIN_HEIGHT);
            let color = match state {
                PillState::Listening => [0, 224, 255, alpha(0.30 + 0.65 * value)],
                PillState::Error => [255, 82, 71, alpha(0.35 + 0.55 * value)],
                PillState::Working | PillState::Idle => [219, 242, 255, alpha(0.22)],
            };
            if let Some(path) =
                capsule_path(bars_x + spec.x, (HEIGHT - height) / 2.0, spec.width, height)
            {
                fill_capsule(&mut pixmap, path, color, transform);
            }
        }
    }
    // Every bar is centered on the horizontal axis. Normalize subpixel
    // coverage so the top and bottom halves remain byte-identical at any DPI.
    symmetrize_vertical(&mut pixmap);

    Some(pixmap.take())
}

fn alpha(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn fill_capsule(pixmap: &mut Pixmap, path: Path, rgba: [u8; 4], transform: Transform) {
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]));
    pixmap.fill_path(&path, &paint, FillRule::Winding, transform, None);
}

fn symmetrize_horizontal(pixmap: &mut Pixmap) {
    let width = pixmap.width() as usize;
    let height = pixmap.height() as usize;
    let pixels = pixmap.data_mut();
    for y in 0..height {
        for x in 0..width / 2 {
            average_pixel_pair(pixels, y * width + x, y * width + (width - 1 - x));
        }
    }
}

fn symmetrize_vertical(pixmap: &mut Pixmap) {
    let width = pixmap.width() as usize;
    let height = pixmap.height() as usize;
    let pixels = pixmap.data_mut();
    for y in 0..height / 2 {
        for x in 0..width {
            average_pixel_pair(pixels, y * width + x, (height - 1 - y) * width + x);
        }
    }
}

fn average_pixel_pair(pixels: &mut [u8], first_pixel: usize, second_pixel: usize) {
    let first = first_pixel * 4;
    let second = second_pixel * 4;
    for channel in 0..4 {
        let average =
            (pixels[first + channel] as u16 + pixels[second + channel] as u16).div_ceil(2);
        pixels[first + channel] = average as u8;
        pixels[second + channel] = average as u8;
    }
}

/// Build a single closed capsule path so one rasterizer owns all four edges.
fn capsule_path(x: f32, y: f32, width: f32, height: f32) -> Option<Path> {
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let radius = (height / 2.0).min(width / 2.0);
    let tangent = radius * 0.552_284_8;
    let left = x;
    let right = x + width;
    let top = y;
    let bottom = y + height;

    let mut path = PathBuilder::new();
    path.move_to(left + radius, top);
    path.line_to(right - radius, top);
    path.cubic_to(
        right - radius + tangent,
        top,
        right,
        top + radius - tangent,
        right,
        top + radius,
    );
    path.line_to(right, bottom - radius);
    path.cubic_to(
        right,
        bottom - radius + tangent,
        right - radius + tangent,
        bottom,
        right - radius,
        bottom,
    );
    path.line_to(left + radius, bottom);
    path.cubic_to(
        left + radius - tangent,
        bottom,
        left,
        bottom - radius + tangent,
        left,
        bottom - radius,
    );
    path.line_to(left, top + radius);
    path.cubic_to(
        left,
        top + radius - tangent,
        left + radius - tangent,
        top,
        left + radius,
        top,
    );
    path.close();
    path.finish()
}

/// Horizontal bar area for a window of the given width: `(x_offset,
/// width)`. One margin on each side keeps both pill ends identical.
pub fn bars_area(total_width: f32) -> (f32, f32) {
    (BAR_MARGIN, (total_width - 2.0 * BAR_MARGIN).max(0.0))
}

/// Horizontal layout of one bar per value: even pitch across the area with
/// neighbor-averaged values so motion stays fluid instead of jagged.
#[derive(Debug, Clone, PartialEq)]
pub struct BarSpec {
    pub x: f32,
    pub width: f32,
    pub value: f32,
}

pub fn bar_specs(values: &[f32], area_width: f32, bar_count: usize) -> Vec<BarSpec> {
    if bar_count == 0 || area_width <= 0.0 {
        return Vec::new();
    }
    let pitch = area_width / bar_count as f32;
    let width = (pitch * 0.5).clamp(1.0, 4.0);
    let start = values.len().saturating_sub(bar_count);
    let window = &values[start..];
    let pad = bar_count.saturating_sub(window.len());
    (0..bar_count)
        .map(|index| {
            let value = if index < pad || window.is_empty() {
                0.0
            } else {
                let position = index - pad;
                let at = |offset: usize| window[offset.min(window.len() - 1)];
                (at(position.saturating_sub(1)) + at(position) + at(position + 1)) / 3.0
            };
            let mirrored_index = bar_count - 1 - index;
            let left_index = index.min(mirrored_index);
            let left_x = left_index as f32 * pitch + (pitch - width) / 2.0;
            let x = if index <= mirrored_index {
                left_x
            } else {
                area_width - left_x - width
            };
            BarSpec {
                x,
                width,
                value: value.clamp(0.0, 1.0),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{HEIGHT, PillState, WIDTH};

    #[test]
    fn bar_area_keeps_equal_margins_on_both_ends() {
        assert_eq!(super::bars_area(285.0), (12.0, 261.0));
        let (offset, width) = super::bars_area(285.0);
        assert_eq!(285.0 - (offset + width), offset);
        assert_eq!(super::bars_area(10.0), (12.0, 0.0));
    }

    #[test]
    fn bar_specs_cover_the_area_with_even_pitch() {
        let values = vec![0.5; 8];
        let specs = super::bar_specs(&values, 160.0, 8);
        assert_eq!(specs.len(), 8);
        for pair in specs.windows(2) {
            let pitch = pair[1].x - pair[0].x;
            assert!((pitch - 20.0).abs() < 0.001);
            assert_eq!(pair[0].width, pair[1].width);
        }
        for spec in &specs {
            assert!(spec.x >= 0.0);
            assert!(spec.x + spec.width <= 160.0 + 0.001);
        }
    }

    #[test]
    fn bar_specs_pad_short_histories_and_clamp() {
        let specs = super::bar_specs(&[2.0, -1.0], 100.0, 4);
        assert_eq!(specs.len(), 4);
        assert_eq!(specs[0].value, 0.0);
        assert_eq!(specs[1].value, 0.0);
        for spec in &specs {
            assert!((0.0..=1.0).contains(&spec.value));
        }
        assert!(super::bar_specs(&[0.5], 100.0, 0).is_empty());
        assert!(super::bar_specs(&[0.5], 0.0, 4).is_empty());
    }

    #[test]
    fn bar_specs_smooth_neighbors() {
        let specs = super::bar_specs(&[0.0, 0.3, 0.6, 0.9], 80.0, 4);
        assert!((specs[0].value - 0.1).abs() < 0.0001);
        assert!((specs[1].value - 0.3).abs() < 0.0001);
        assert!((specs[2].value - 0.6).abs() < 0.0001);
        assert!((specs[3].value - 0.8).abs() < 0.0001);
    }

    #[test]
    fn layered_bitmap_has_transparent_corners_and_symmetric_edges() {
        let width = (WIDTH * 1.25).round() as u32;
        let height = (HEIGHT * 1.25).round() as u32;
        let pixels =
            super::render_rgba(width, height, &[], PillState::Working).expect("pill should render");
        let pixel_at = |x: u32, y: u32| {
            let offset = ((y * width + x) * 4) as usize;
            &pixels[offset..offset + 4]
        };

        assert_eq!(pixel_at(0, 0)[3], 0);
        assert_eq!(pixel_at(width - 1, 0)[3], 0);
        assert_eq!(pixel_at(0, height - 1)[3], 0);
        assert_eq!(pixel_at(width - 1, height - 1)[3], 0);

        for y in 0..height {
            for x in 0..width {
                assert_eq!(pixel_at(x, y), pixel_at(x, height - 1 - y));
            }
        }
        for y in 0..height {
            for x in 0..width {
                assert_eq!(pixel_at(x, y), pixel_at(width - 1 - x, y), "x={x} y={y}");
            }
        }
    }

    #[test]
    fn waveform_is_exactly_centered_vertically() {
        let width = (WIDTH * 1.25).round() as u32;
        let height = (HEIGHT * 1.25).round() as u32;
        let bars: Vec<f32> = (0..64).map(|index| index as f32 / 63.0).collect();
        let pixels = super::render_rgba(width, height, &bars, PillState::Listening)
            .expect("waveform should render");
        let pixel_at = |x: u32, y: u32| {
            let offset = ((y * width + x) * 4) as usize;
            &pixels[offset..offset + 4]
        };
        for y in 0..height {
            for x in 0..width {
                assert_eq!(pixel_at(x, y), pixel_at(x, height - 1 - y));
            }
        }
    }

    #[test]
    fn equal_waveform_values_keep_equal_end_padding() {
        let width = (WIDTH * 1.25).round() as u32;
        let height = (HEIGHT * 1.25).round() as u32;
        let pixels = super::render_rgba(width, height, &[0.5; 64], PillState::Listening)
            .expect("waveform should render");
        let base = super::render_rgba(width, height, &[], PillState::Listening)
            .expect("base pill should render");
        let changed_columns: Vec<u32> = (0..width)
            .filter(|&x| {
                (0..height).any(|y| {
                    let offset = ((y * width + x) * 4) as usize;
                    pixels[offset..offset + 4] != base[offset..offset + 4]
                })
            })
            .collect();
        let first = *changed_columns.first().expect("waveform has pixels");
        let last = *changed_columns.last().expect("waveform has pixels");
        assert_eq!(first, width - 1 - last);
    }
}
