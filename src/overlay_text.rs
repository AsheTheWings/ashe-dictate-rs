//! Text rasterization for the dictation overlay.
//!
//! This module owns the cosmic-text rendering behind the layered pill: bar
//! labels, the typing preview with its line-break marker styling, and the
//! pixel compositing helpers. It has no Win32 dependencies so the renderer
//! stays unit-testable on any host; `native_overlay` owns presentation.

use crate::pill_renderer::{self, TopBarAlignment, TopBarContent};
use cosmic_text::{Attrs, Buffer, Color as TextColor, Family, FontSystem, Metrics, Shaping};
use cosmic_text::{SwashCache, Wrap};

const MAIN_FONT_PIXELS: f32 = 14.0;
const TOP_BAR_FONT_PIXELS: f32 = 13.2;
/// Line-break marker scale and color. The `↵` renders larger than the
/// surrounding bar text in the palette cyan, both as listening feedback
/// and inline in the typing preview.
const TOP_BAR_LINE_BREAK_SCALE: f32 = 1.5;
const LINE_BREAK_CYAN: TextColor = TextColor::rgb(0, 224, 255);
const LINE_BREAK_MARKER: char = '↵';

pub(crate) struct TextRasterizer {
    fonts: FontSystem,
    cache: SwashCache,
}

impl TextRasterizer {
    pub(crate) fn new() -> Self {
        Self {
            fonts: FontSystem::new(),
            cache: SwashCache::new(),
        }
    }

    pub(crate) fn draw(
        &mut self,
        rgba: &mut [u8],
        width: u32,
        height: u32,
        scale: f32,
        main_text: Option<&str>,
        top_bar: Option<&TopBarContent>,
    ) {
        if let Some(text) = main_text.filter(|text| !text.is_empty()) {
            let rect = PixelRect::from_logical(
                0.0,
                pill_renderer::MAIN_TOP,
                pill_renderer::WIDTH,
                pill_renderer::PILL_HEIGHT,
                scale,
            );
            self.draw_text(
                rgba,
                width,
                height,
                text,
                MAIN_FONT_PIXELS * scale,
                rect,
                TextAlign::Center,
                TextColor::rgb(255, 255, 255),
                u8::MAX,
            );
        }
        if let Some(content) = top_bar {
            let count_left = pill_renderer::TOP_BAR_X + pill_renderer::TOP_BAR_TEXT_INSET;
            let count_rect = PixelRect::from_logical(
                count_left,
                pill_renderer::TOP_BAR_Y,
                pill_renderer::TOP_BAR_COUNT_WIDTH,
                pill_renderer::TOP_BAR_HEIGHT,
                scale,
            );
            self.draw_text(
                rgba,
                width,
                height,
                &content.count,
                TOP_BAR_FONT_PIXELS * scale,
                count_rect,
                TextAlign::Left,
                TextColor::rgb(255, 255, 255),
                204,
            );
            // Both centered status (timer, silence notice) and trailing typed
            // preview share the region right of the insertion count so the
            // two text runs can never overlap. Centered status stays
            // optically centered in the remaining space.
            // Centered status (timer, silence notice) is centered in the
            // full bar so its midpoint matches the bar midpoint, with the
            // insertion count balanced by equal space on the right. At the
            // current strings and sizes this clears the count without
            // overlap. The typed preview stays in the region right of the
            // count, right-aligned.
            let (content_rect, alignment) = match content.alignment {
                TopBarAlignment::Center => (
                    PixelRect::from_logical(
                        pill_renderer::TOP_BAR_X + pill_renderer::TOP_BAR_TEXT_INSET,
                        pill_renderer::TOP_BAR_Y,
                        pill_renderer::TOP_BAR_WIDTH - 2.0 * pill_renderer::TOP_BAR_TEXT_INSET,
                        pill_renderer::TOP_BAR_HEIGHT,
                        scale,
                    ),
                    TextAlign::Center,
                ),
                TopBarAlignment::Trailing => {
                    let content_left = count_left
                        + pill_renderer::TOP_BAR_COUNT_WIDTH
                        + pill_renderer::TOP_BAR_CONTENT_GAP;
                    let content_right = pill_renderer::TOP_BAR_X + pill_renderer::TOP_BAR_WIDTH
                        - pill_renderer::TOP_BAR_TEXT_INSET;
                    (
                        PixelRect::from_logical(
                            content_left,
                            pill_renderer::TOP_BAR_Y,
                            (content_right - content_left).max(0.0),
                            pill_renderer::TOP_BAR_HEIGHT,
                            scale,
                        ),
                        TextAlign::Right,
                    )
                }
            };
            // The typing preview keeps `↵` markers inline as larger cyan
            // spans with a space of padding on each side. The padding is
            // display-only; the committed insertion still holds `\n`.
            if content.alignment == TopBarAlignment::Trailing
                && content.content.contains(LINE_BREAK_MARKER)
            {
                self.draw_line_break_preview(
                    rgba,
                    width,
                    height,
                    &content.content,
                    TOP_BAR_FONT_PIXELS * scale,
                    content_rect,
                    alignment,
                    204,
                );
            } else {
                self.draw_text(
                    rgba,
                    width,
                    height,
                    &content.content,
                    TOP_BAR_FONT_PIXELS * scale,
                    content_rect,
                    alignment,
                    TextColor::rgb(255, 255, 255),
                    204,
                );
            }
            if let Some(accessory) = content.accessory.as_deref() {
                let accessory_right = pill_renderer::TOP_BAR_X + pill_renderer::TOP_BAR_WIDTH
                    - pill_renderer::TOP_BAR_TEXT_INSET;
                let accessory_rect = PixelRect::from_logical(
                    accessory_right - pill_renderer::TOP_BAR_ACCESSORY_WIDTH,
                    pill_renderer::TOP_BAR_Y,
                    pill_renderer::TOP_BAR_ACCESSORY_WIDTH,
                    pill_renderer::TOP_BAR_HEIGHT,
                    scale,
                );
                self.draw_text(
                    rgba,
                    width,
                    height,
                    accessory,
                    TOP_BAR_FONT_PIXELS * TOP_BAR_LINE_BREAK_SCALE * scale,
                    accessory_rect,
                    TextAlign::Center,
                    LINE_BREAK_CYAN,
                    204,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text(
        &mut self,
        rgba: &mut [u8],
        width: u32,
        height: u32,
        text: &str,
        font_size: f32,
        clip: PixelRect,
        alignment: TextAlign,
        color: TextColor,
        opacity: u8,
    ) {
        if text.is_empty() || clip.width <= 0 || clip.height <= 0 {
            return;
        }
        let line_height = (font_size * 1.35).max(1.0);
        let mut buffer = Buffer::new(
            &mut self.fonts,
            Metrics::new(font_size.max(1.0), line_height),
        );
        buffer.set_wrap(&mut self.fonts, Wrap::None);
        buffer.set_size(&mut self.fonts, None, Some(clip.height as f32));
        let attrs = Attrs::new().family(Family::Name("Segoe UI"));
        buffer.set_text(&mut self.fonts, text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.fonts, false);

        let mut raster = Vec::new();
        buffer.draw(
            &mut self.fonts,
            &mut self.cache,
            color,
            |x, y, pixel_width, pixel_height, color| {
                raster.push(RasterPixel {
                    x,
                    y,
                    width: pixel_width,
                    height: pixel_height,
                    color,
                });
            },
        );
        Self::blit(rgba, width, height, &raster, clip, alignment, opacity);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_line_break_preview(
        &mut self,
        rgba: &mut [u8],
        width: u32,
        height: u32,
        preview: &str,
        font_size: f32,
        clip: PixelRect,
        alignment: TextAlign,
        opacity: u8,
    ) {
        let spans = line_break_spans(preview, font_size);
        let refs: Vec<(&str, Attrs)> = spans
            .iter()
            .map(|(text, attrs)| (text.as_str(), attrs.clone()))
            .collect();
        self.draw_rich_text(
            rgba,
            width,
            height,
            &refs,
            font_size,
            clip,
            alignment,
            TextColor::rgb(255, 255, 255),
            opacity,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_rich_text(
        &mut self,
        rgba: &mut [u8],
        width: u32,
        height: u32,
        spans: &[(&str, Attrs)],
        base_font_size: f32,
        clip: PixelRect,
        alignment: TextAlign,
        default_color: TextColor,
        opacity: u8,
    ) {
        if spans.is_empty() || clip.width <= 0 || clip.height <= 0 {
            return;
        }
        // The buffer line must clear the enlarged marker, not just the
        // base text, or the marker clips vertically.
        let line_height = (base_font_size * TOP_BAR_LINE_BREAK_SCALE * 1.35).max(1.0);
        let mut buffer = Buffer::new(
            &mut self.fonts,
            Metrics::new(base_font_size.max(1.0), line_height),
        );
        buffer.set_wrap(&mut self.fonts, Wrap::None);
        buffer.set_size(&mut self.fonts, None, Some(clip.height as f32));
        let default_attrs = Attrs::new().family(Family::Name("Segoe UI"));
        buffer.set_rich_text(
            &mut self.fonts,
            spans.iter().map(|(text, attrs)| (*text, attrs.clone())),
            &default_attrs,
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut self.fonts, false);

        let mut raster = Vec::new();
        buffer.draw(
            &mut self.fonts,
            &mut self.cache,
            default_color,
            |x, y, pixel_width, pixel_height, color| {
                raster.push(RasterPixel {
                    x,
                    y,
                    width: pixel_width,
                    height: pixel_height,
                    color,
                });
            },
        );
        Self::blit(rgba, width, height, &raster, clip, alignment, opacity);
    }

    fn blit(
        rgba: &mut [u8],
        width: u32,
        height: u32,
        raster: &[RasterPixel],
        clip: PixelRect,
        alignment: TextAlign,
        opacity: u8,
    ) {
        let Some(bounds) = RasterBounds::for_pixels(raster) else {
            return;
        };
        let origin_x = match alignment {
            TextAlign::Left => clip.left - bounds.left,
            TextAlign::Center => clip.left + (clip.width - bounds.width()) / 2 - bounds.left,
            TextAlign::Right => clip.right() - bounds.width() - bounds.left,
        };
        let origin_y = clip.top + (clip.height - bounds.height()) / 2 - bounds.top;

        for pixel in raster {
            for offset_y in 0..pixel.height as i32 {
                for offset_x in 0..pixel.width as i32 {
                    let x = origin_x + pixel.x + offset_x;
                    let y = origin_y + pixel.y + offset_y;
                    if clip.contains(x, y) {
                        composite_pixel(rgba, width, height, x, y, pixel.color, opacity);
                    }
                }
            }
        }
    }
}

/// Split a typing preview into render spans. Base text keeps the default
/// style; each `↵` becomes a larger cyan span wrapped in one space of
/// horizontal padding on each side.
fn line_break_spans(preview: &str, base_font_size: f32) -> Vec<(String, Attrs<'_>)> {
    let base = Attrs::new().family(Family::Name("Segoe UI"));
    let marker_font = (base_font_size * TOP_BAR_LINE_BREAK_SCALE).max(1.0);
    let marker = Attrs::new()
        .family(Family::Name("Segoe UI"))
        .color(LINE_BREAK_CYAN)
        .metrics(Metrics::new(marker_font, (marker_font * 1.35).max(1.0)));
    let mut spans = Vec::new();
    for (index, segment) in preview.split(LINE_BREAK_MARKER).enumerate() {
        if index > 0 {
            spans.push((" ".to_string(), base.clone()));
            spans.push((LINE_BREAK_MARKER.to_string(), marker.clone()));
            spans.push((" ".to_string(), base.clone()));
        }
        if !segment.is_empty() {
            spans.push((segment.to_string(), base.clone()));
        }
    }
    spans
}

#[derive(Clone, Copy)]
enum TextAlign {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy)]
struct PixelRect {
    left: i32,
    top: i32,
    width: i32,
    height: i32,
}

impl PixelRect {
    fn from_logical(x: f32, y: f32, width: f32, height: f32, scale: f32) -> Self {
        Self {
            left: (x * scale).round() as i32,
            top: (y * scale).round() as i32,
            width: (width * scale).round() as i32,
            height: (height * scale).round() as i32,
        }
    }

    fn right(self) -> i32 {
        self.left + self.width
    }

    fn contains(self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right() && y >= self.top && y < self.top + self.height
    }
}

struct RasterPixel {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    color: TextColor,
}

struct RasterBounds {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl RasterBounds {
    fn for_pixels(pixels: &[RasterPixel]) -> Option<Self> {
        let first = pixels.first()?;
        let mut bounds = Self {
            left: first.x,
            top: first.y,
            right: first.x + first.width as i32,
            bottom: first.y + first.height as i32,
        };
        for pixel in &pixels[1..] {
            bounds.left = bounds.left.min(pixel.x);
            bounds.top = bounds.top.min(pixel.y);
            bounds.right = bounds.right.max(pixel.x + pixel.width as i32);
            bounds.bottom = bounds.bottom.max(pixel.y + pixel.height as i32);
        }
        Some(bounds)
    }

    fn width(&self) -> i32 {
        self.right - self.left
    }

    fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

fn composite_pixel(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    color: TextColor,
    opacity: u8,
) {
    if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
        return;
    }
    let index = (y as usize * width as usize + x as usize) * 4;
    let source_alpha = (color.a() as u16 * opacity as u16 / 255) as u8;
    let inverse = 255_u16 - source_alpha as u16;
    let source = [color.r(), color.g(), color.b()];
    for channel in 0..3 {
        let source_premultiplied = source[channel] as u16 * source_alpha as u16 / 255;
        rgba[index + channel] =
            (source_premultiplied + rgba[index + channel] as u16 * inverse / 255).min(255) as u8;
    }
    rgba[index + 3] = (source_alpha as u16 + rgba[index + 3] as u16 * inverse / 255).min(255) as u8;
}
#[cfg(test)]
mod tests {
    use super::{LINE_BREAK_CYAN, composite_pixel, line_break_spans};
    use cosmic_text::Color;

    #[test]
    fn text_compositing_creates_premultiplied_alpha_on_transparency() {
        let mut rgba = [0_u8; 4];
        composite_pixel(&mut rgba, 1, 1, 0, 0, Color::rgba(255, 255, 255, 128), 128);
        assert_eq!(rgba, [64, 64, 64, 64]);
    }

    #[test]
    fn text_compositing_obeys_the_clip_surface_bounds() {
        let mut rgba = [0_u8; 4];
        composite_pixel(&mut rgba, 1, 1, 1, 0, Color::rgb(255, 255, 255), 255);
        assert_eq!(rgba, [0, 0, 0, 0]);
    }

    #[test]
    fn line_break_preview_pads_the_marker_and_marks_it_cyan() {
        let spans = line_break_spans("ab↵cd", 13.2);
        let texts: Vec<&str> = spans.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(texts, ["ab", " ", "↵", " ", "cd"]);
        assert!(spans[0].1.color_opt.is_none());
        assert!(spans[0].1.metrics_opt.is_none());
        assert_eq!(spans[2].1.color_opt, Some(LINE_BREAK_CYAN));
        assert!(spans[2].1.metrics_opt.is_some());
    }
}
