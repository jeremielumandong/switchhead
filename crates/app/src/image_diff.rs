//! Image diffs (side by side, swipe, onion skin) and what is shown for other binary
//! files: their kind (from the first bytes) and sizes.
//!
//! Images are decoded by GPUI when painted; [`image_size`] reads the pixel size from the
//! file header so the info line needs no decode.

use std::path::Path;
use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, BorrowAppContext as _, Bounds, Global, Image, ImageFormat,
    InteractiveElement as _, IntoElement, MouseMoveEvent, ObjectFit, ParentElement as _, Pixels,
    SharedString, Styled as _, StyledImage as _, Window, canvas, div, img, px, relative,
};

use crate::appearance::{rpx, ts};
use crate::theme::{MONO, Palette};
use crate::ui;

/// Images larger than this are shown as binary files.
pub const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;

/// How two versions of an image are compared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImageMode {
    /// Old and new next to each other.
    #[default]
    SideBySide,
    /// New over old, revealed up to the pointer.
    Swipe,
    /// New over old, as opaque as the pointer is far right.
    Onion,
}

/// The image view's mode and slider, shared by every diff while the app runs.
#[derive(Clone, Copy, Debug)]
pub struct ImageView {
    /// Comparison mode.
    pub mode: ImageMode,
    /// Swipe position or onion opacity, 0..=1.
    pub mix: f32,
    /// Where the comparison was last painted (window coordinates).
    stage: Bounds<Pixels>,
}

impl Default for ImageView {
    fn default() -> Self {
        Self {
            mode: ImageMode::default(),
            mix: 0.5,
            stage: Bounds::default(),
        }
    }
}

impl Global for ImageView {}

/// One version of an image.
#[derive(Clone, Debug)]
pub struct ImageSide {
    /// The bytes, for GPUI to decode.
    pub image: Arc<Image>,
    /// File size.
    pub bytes: u64,
    /// Width and height in pixels, when the header says.
    pub dims: Option<(u32, u32)>,
}

impl ImageSide {
    fn new(format: ImageFormat, bytes: &[u8]) -> Self {
        Self {
            dims: image_size(bytes),
            bytes: bytes.len() as u64,
            image: Arc::new(Image::from_bytes(format, bytes.to_vec())),
        }
    }
}

/// The image format GPUI can show for `path`, by extension.
pub fn format_of(path: &Path) -> Option<ImageFormat> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::Webp,
        "bmp" => ImageFormat::Bmp,
        "tif" | "tiff" => ImageFormat::Tiff,
        "ico" => ImageFormat::Ico,
        "svg" => ImageFormat::Svg,
        _ => return None,
    })
}

/// Both versions of `path` as images, if it is an image GPUI can show and neither
/// version is too large.
pub fn sides(
    path: &Path,
    old: Option<&[u8]>,
    new: Option<&[u8]>,
) -> Option<(Option<ImageSide>, Option<ImageSide>)> {
    let format = format_of(path)?;
    if old.is_none() && new.is_none() {
        return None;
    }
    if [old, new]
        .iter()
        .flatten()
        .any(|b| b.len() > MAX_IMAGE_BYTES)
    {
        return None;
    }
    Some((
        old.map(|b| ImageSide::new(format, b)),
        new.map(|b| ImageSide::new(format, b)),
    ))
}

/// Pixel width and height from a PNG, GIF, BMP, JPEG or WebP header.
pub fn image_size(b: &[u8]) -> Option<(u32, u32)> {
    let be32 = |i: usize| Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?));
    let le16 = |i: usize| Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?) as u32);
    let be16 = |i: usize| Some(u16::from_be_bytes(b.get(i..i + 2)?.try_into().ok()?) as u32);
    let le32 = |i: usize| Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?));
    if b.starts_with(b"\x89PNG\r\n\x1a\n") && b.get(12..16) == Some(b"IHDR") {
        return Some((be32(16)?, be32(20)?));
    }
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        return Some((le16(6)?, le16(8)?));
    }
    if b.starts_with(b"BM") {
        let h = le32(22)? as i32;
        return Some((le32(18)?, h.unsigned_abs()));
    }
    if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        return match b.get(12..16)? {
            b"VP8 " => Some((le16(26)? & 0x3fff, le16(28)? & 0x3fff)),
            b"VP8L" => {
                let v = le32(21)?;
                Some(((v & 0x3fff) + 1, ((v >> 14) & 0x3fff) + 1))
            }
            b"VP8X" => {
                let le24 = |i: usize| -> Option<u32> {
                    let s = b.get(i..i + 3)?;
                    Some(s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16)
                };
                Some((le24(24)? + 1, le24(27)? + 1))
            }
            _ => None,
        };
    }
    if b.starts_with(&[0xff, 0xd8]) {
        // Walk the segments to the first start-of-frame marker.
        let mut i = 2;
        while i + 9 < b.len() {
            if b[i] != 0xff {
                return None;
            }
            let marker = b[i + 1];
            if marker == 0xff {
                i += 1;
                continue;
            }
            let len = be16(i + 2)? as usize;
            let sof = matches!(marker, 0xc0..=0xcf) && !matches!(marker, 0xc4 | 0xc8 | 0xcc);
            if sof {
                return Some((be16(i + 7)?, be16(i + 5)?));
            }
            i += 2 + len;
        }
    }
    None
}

/// What kind of file `head` (its first bytes) starts, for the binary file note.
pub fn kind_of(head: &[u8]) -> Option<&'static str> {
    const MAGIC: &[(&[u8], &str)] = &[
        (b"%PDF", "PDF document"),
        (b"PK\x03\x04", "ZIP archive"),
        (b"\x1f\x8b", "gzip archive"),
        (b"7z\xbc\xaf\x27\x1c", "7-Zip archive"),
        (b"\xfd7zXZ\x00", "xz archive"),
        (b"\x28\xb5\x2f\xfd", "Zstandard archive"),
        (b"BZh", "bzip2 archive"),
        (b"\x7fELF", "ELF binary"),
        (b"MZ", "Windows executable"),
        (b"\xcf\xfa\xed\xfe", "Mach-O binary"),
        (b"\xca\xfe\xba\xbe", "Mach-O universal binary"),
        (b"\x00asm", "WebAssembly module"),
        (b"SQLite format 3\x00", "SQLite database"),
        (b"\x89PNG", "PNG image"),
        (b"\xff\xd8\xff", "JPEG image"),
        (b"GIF8", "GIF image"),
        (b"OTTO", "OpenType font"),
        (b"\x00\x01\x00\x00", "TrueType font"),
        (b"wOFF", "WOFF font"),
        (b"wOF2", "WOFF2 font"),
        (b"ID3", "MP3 audio"),
        (b"OggS", "Ogg media"),
        (b"fLaC", "FLAC audio"),
        (b"\x1aE\xdf\xa3", "Matroska / WebM video"),
    ];
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        return Some("MP4 / QuickTime media");
    }
    if head.starts_with(b"RIFF") && head.len() >= 12 {
        return match &head[8..12] {
            b"WEBP" => Some("WebP image"),
            b"WAVE" => Some("WAV audio"),
            b"AVI " => Some("AVI video"),
            _ => None,
        };
    }
    MAGIC
        .iter()
        .find(|(m, _)| head.starts_with(m))
        .map(|(_, k)| *k)
}

fn side_info(s: &ImageSide) -> String {
    match s.dims {
        Some((w, h)) => format!("{w} × {h} · {}", ui::bytes(s.bytes)),
        None => ui::bytes(s.bytes),
    }
}

fn set_mode(mode: ImageMode, cx: &mut App) {
    cx.update_global::<ImageView, _>(|v, _| v.mode = mode);
    cx.refresh_windows();
}

/// The comparison for an image diff: an info line with the mode switch, then the images.
pub fn render(
    old: Option<&ImageSide>,
    new: Option<&ImageSide>,
    p: &Palette,
    cx: &App,
) -> AnyElement {
    let view = *cx.global::<ImageView>();
    let both = old.is_some() && new.is_some();
    let mode = if both {
        view.mode
    } else {
        ImageMode::SideBySide
    };
    let info = match (old, new) {
        (Some(o), Some(n)) => format!("{}  →  {}", side_info(o), side_info(n)),
        (None, Some(n)) => format!("Added · {}", side_info(n)),
        (Some(o), None) => format!("Deleted · {}", side_info(o)),
        (None, None) => String::new(),
    };
    let mode_button = |label: &'static str, m: ImageMode| -> (SharedString, bool, ui::OnClick) {
        (
            label.into(),
            mode == m,
            Box::new(move |_, _, cx: &mut App| set_mode(m, cx)),
        )
    };
    let bar = div()
        .flex_none()
        .h(rpx(32.))
        .px(rpx(12.))
        .flex()
        .items_center()
        .gap(rpx(10.))
        .border_b_1()
        .border_color(p.bd)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(MONO)
                .text_size(ts::SMALL)
                .text_color(p.fg2)
                .child(info),
        )
        .when(both && mode != ImageMode::SideBySide, |d| {
            d.child(
                div()
                    .text_size(ts::SMALL)
                    .text_color(p.fg3)
                    .child("Move the pointer over the image"),
            )
        })
        .when(both, |d| {
            d.child(ui::segmented(
                "image-mode",
                vec![
                    mode_button("Side by side", ImageMode::SideBySide),
                    mode_button("Swipe", ImageMode::Swipe),
                    mode_button("Onion skin", ImageMode::Onion),
                ],
                20.,
                p,
            ))
        });
    let picture = |s: &ImageSide| {
        img(s.image.clone())
            .object_fit(ObjectFit::Contain)
            .size_full()
    };
    let checker = p.panel;
    let stage = match (mode, old, new) {
        (ImageMode::SideBySide, _, _) | (_, None, _) | (_, _, None) => {
            let column = |label: &'static str, s: Option<&ImageSide>, color| {
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .gap(rpx(6.))
                    .p(rpx(12.))
                    .child(
                        div()
                            .flex_none()
                            .text_size(ts::SMALL)
                            .text_color(color)
                            .child(label),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .bg(checker)
                            .rounded(px(4.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(ts::SMALL)
                            .text_color(p.fg3)
                            .map(|d| match s {
                                Some(s) => d.child(picture(s)),
                                None => d.child("No image"),
                            }),
                    )
            };
            div()
                .size_full()
                .flex()
                .child(column("Before", old, p.deleted))
                .child(column("After", new, p.added))
                .into_any_element()
        }
        (ImageMode::Swipe, Some(o), Some(n)) => {
            let mix = view.mix.clamp(0.001, 1.0);
            div()
                .relative()
                .size_full()
                .bg(checker)
                .child(div().absolute().inset_0().child(picture(o)))
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .h_full()
                        .w(relative(mix))
                        .overflow_hidden()
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .h_full()
                                .w(relative(1. / mix))
                                .child(picture(n)),
                        ),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .h_full()
                        .left(relative(mix))
                        .w(px(2.))
                        .bg(p.acc),
                )
                .into_any_element()
        }
        (ImageMode::Onion, Some(o), Some(n)) => div()
            .relative()
            .size_full()
            .bg(checker)
            .child(div().absolute().inset_0().child(picture(o)))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .opacity(view.mix.clamp(0., 1.))
                    .child(picture(n)),
            )
            .into_any_element(),
    };
    let tracks = both && mode != ImageMode::SideBySide;
    div()
        .size_full()
        .flex()
        .flex_col()
        .child(bar)
        .child(
            div()
                .id("image-stage")
                .flex_1()
                .min_h_0()
                .p(rpx(12.))
                .relative()
                .child(stage)
                .when(tracks, |d| {
                    d.child(
                        canvas(
                            |bounds, _, cx: &mut App| {
                                cx.update_global::<ImageView, _>(|v, _| v.stage = bounds)
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    )
                    .on_mouse_move(
                        |ev: &MouseMoveEvent, window: &mut Window, cx: &mut App| {
                            let b = cx.global::<ImageView>().stage;
                            let w = f32::from(b.size.width);
                            if w <= 0. {
                                return;
                            }
                            let mix = (f32::from(ev.position.x - b.origin.x) / w).clamp(0., 1.);
                            cx.update_global::<ImageView, _>(|v, _| v.mix = mix);
                            window.refresh();
                        },
                    )
                }),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_image_sizes() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend(640u32.to_be_bytes());
        png.extend(480u32.to_be_bytes());
        assert_eq!(image_size(&png), Some((640, 480)));
        let gif = b"GIF89a\x20\x00\x10\x00";
        assert_eq!(image_size(gif), Some((32, 16)));
        let mut bmp = vec![0u8; 26];
        bmp[..2].copy_from_slice(b"BM");
        bmp[18..22].copy_from_slice(&3u32.to_le_bytes());
        bmp[22..26].copy_from_slice(&(-2i32).to_le_bytes());
        assert_eq!(image_size(&bmp), Some((3, 2)));
        // JPEG: SOI, an APP0 segment, then SOF0 with height 2, width 5.
        let jpg = [
            0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x00,
            0x02, 0x00, 0x05, 0x03, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(image_size(&jpg), Some((5, 2)));
        assert_eq!(image_size(b"not an image"), None);
    }

    #[test]
    fn knows_file_kinds() {
        assert_eq!(kind_of(b"%PDF-1.7"), Some("PDF document"));
        assert_eq!(kind_of(b"\x7fELF\x02"), Some("ELF binary"));
        assert_eq!(
            kind_of(b"\0\0\0\x18ftypmp42"),
            Some("MP4 / QuickTime media")
        );
        assert_eq!(kind_of(b"\0\x01\x02"), None);
        assert_eq!(format_of(Path::new("a/B.JPG")), Some(ImageFormat::Jpeg));
        assert_eq!(format_of(Path::new("a.txt")), None);
    }
}
