//! Inline images in the preview, drawn by [`ratatui_image`]: with the kitty
//! graphics protocol (its Unicode placeholders) where the terminal has it,
//! else Sixel or iTerm2's protocol. A terminal with none of them (or a
//! multiplexer that hides them) keeps the text preview: half-block images
//! are not used.
//!
//! The preview reserves blank rows for each image in its text (see
//! [`crate::markdown::render_with`]) and draws the image over them with a
//! [`SlicedImage`], so an image scrolls with the text, and shows in part when
//! it is cut by the top or the bottom of the view.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use image::DynamicImage;
use ratatui::{
    buffer::Buffer,
    layout::{Rect, Size},
    widgets::Widget,
};
use ratatui_image::{
    Resize,
    picker::{Picker, ProtocolType},
    sliced::{SignedPosition, SlicedImage, SlicedProtocol},
};

/// Decoded images are scaled down to at most this many pixels a side: more
/// than any terminal shows, and it bounds the memory they take.
const MAX_PIXELS: u32 = 2048;

/// The images of the preview, decoded once and encoded for the terminal once
/// per size they are shown at.
pub struct Images {
    picker: Picker,
    /// By canonical path; `None` for a file that cannot be shown.
    decoded: HashMap<PathBuf, Option<DynamicImage>>,
    /// By canonical path and the largest size asked for.
    encoded: HashMap<(PathBuf, Size), Option<SlicedProtocol>>,
}

impl Images {
    pub fn new(picker: Picker) -> Self {
        Images {
            picker,
            decoded: HashMap::new(),
            encoded: HashMap::new(),
        }
    }

    /// The size in cells the image at `path` is shown at: its natural size, or
    /// scaled down (keeping its aspect ratio) to fit `max`. `None` if it cannot
    /// be shown.
    pub fn size(&mut self, path: &Path, max: Size) -> Option<Size> {
        self.protocol(path, max).map(SlicedProtocol::size)
    }

    /// Draw the image at `path` (at the size [`Images::size`] gave for `max`)
    /// with its top left corner at `position`, relative to `area`: the rows
    /// outside `area` are left out.
    pub fn render(
        &mut self,
        path: &Path,
        max: Size,
        area: Rect,
        position: SignedPosition,
        buf: &mut Buffer,
    ) {
        if let Some(protocol) = self.protocol(path, max) {
            SlicedImage::new(protocol, position).render(area, buf);
        }
    }

    fn protocol(&mut self, path: &Path, max: Size) -> Option<&SlicedProtocol> {
        if max.width == 0 || max.height == 0 {
            return None;
        }
        let key = (path.canonicalize().ok()?, max);
        if !self.encoded.contains_key(&key) {
            let image = self
                .decoded
                .entry(key.0.clone())
                .or_insert_with(|| decode(&key.0))
                .clone();
            let protocol = image.and_then(|image| {
                SlicedProtocol::new_with_resize(&self.picker, image, max, Resize::Fit(None)).ok()
            });
            self.encoded.insert(key.clone(), protocol);
        }
        self.encoded.get(&key)?.as_ref()
    }

    /// Forget what was sent to the terminal, so images are sent again when next
    /// shown (after the terminal switched screens, which may drop them).
    pub fn forget(&mut self) {
        self.encoded.clear();
    }
}

fn decode(path: &Path) -> Option<DynamicImage> {
    let image = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    if image.width() == 0 || image.height() == 0 {
        return None;
    }
    Some(
        if image.width() > MAX_PIXELS || image.height() > MAX_PIXELS {
            image.thumbnail(MAX_PIXELS, MAX_PIXELS)
        } else {
            image
        },
    )
}

/// Whether to show images, from `ZOLA_TOOL_IMAGES`: `0`/`off` never, `1`/`on`
/// always with the kitty protocol (for a multiplexer that passes images
/// through but not the terminal's replies), otherwise when the terminal says
/// it can.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Setting {
    Auto,
    On,
    Off,
}

fn setting(value: Option<&str>) -> Setting {
    match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("0" | "off" | "no" | "false") => Setting::Off,
        Some("1" | "on" | "yes" | "true" | "kitty") => Setting::On,
        _ => Setting::Auto,
    }
}

/// The protocol to draw images with, if any, for what the terminal answered.
fn protocol_for(setting: Setting, detected: ProtocolType) -> Option<ProtocolType> {
    match (setting, detected) {
        (Setting::Off, _) => None,
        (Setting::On, _) => Some(ProtocolType::Kitty),
        // blocks of colour are a poor preview; the `[image: alt]` text is kept
        (Setting::Auto, ProtocolType::Halfblocks) => None,
        (Setting::Auto, protocol) => Some(protocol),
    }
}

/// Ask the terminal which image protocol it has, and the size of its cells.
/// Call after entering the alternate screen, before the first event is read:
/// the replies are read from stdin.
pub fn detect() -> Option<Images> {
    let setting = setting(std::env::var("ZOLA_TOOL_IMAGES").ok().as_deref());
    if setting == Setting::Off {
        return None;
    }
    let mut picker = Picker::from_query_stdio().ok()?;
    let protocol = protocol_for(setting, picker.protocol_type())?;
    picker.set_protocol_type(protocol);
    Some(Images::new(picker))
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use ratatui_image::FontSize;

    /// A picker for the kitty protocol with 10x20 pixel cells, without asking
    /// a terminal.
    pub fn kitty_picker() -> Picker {
        #[allow(deprecated)] // the only constructor that does not query
        let mut picker = Picker::from_fontsize(FontSize::new(10, 20));
        picker.set_protocol_type(ProtocolType::Kitty);
        picker
    }

    fn scratch() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "zola-tool-images-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_setting_overrides_detection() {
        assert_eq!(setting(None), Setting::Auto);
        assert_eq!(setting(Some("off")), Setting::Off);
        assert_eq!(setting(Some(" ON ")), Setting::On);
        assert_eq!(setting(Some("auto")), Setting::Auto);
        use ProtocolType::*;
        assert_eq!(protocol_for(Setting::Auto, Kitty), Some(Kitty));
        assert_eq!(protocol_for(Setting::Auto, Sixel), Some(Sixel));
        assert_eq!(protocol_for(Setting::Auto, Iterm2), Some(Iterm2));
        assert_eq!(protocol_for(Setting::Auto, Halfblocks), None);
        assert_eq!(protocol_for(Setting::On, Halfblocks), Some(Kitty));
        assert_eq!(protocol_for(Setting::Off, Kitty), None);
    }

    #[test]
    fn images_show_at_natural_size_or_smaller() {
        let dir = scratch();
        let path = dir.join("pic.png");
        // 200x100 px at 10x20 px cells: 20x5 cells
        image::RgbImage::new(200, 100).save(&path).unwrap();
        let mut images = Images::new(kitty_picker());
        assert_eq!(
            images.size(&path, Size::new(80, 40)),
            Some(Size::new(20, 5))
        );
        // half the width: half the size, aspect ratio kept
        assert_eq!(
            images.size(&path, Size::new(10, 40)),
            Some(Size::new(10, 3))
        );
        assert_eq!(images.size(&path, Size::new(0, 40)), None);

        let broken = dir.join("broken.png");
        std::fs::write(&broken, b"not an image").unwrap();
        assert_eq!(images.size(&broken, Size::new(80, 40)), None);
        assert_eq!(
            images.size(&dir.join("missing.png"), Size::new(80, 40)),
            None
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn large_images_are_scaled_down_when_decoded() {
        let dir = scratch();
        let path = dir.join("big.jpg");
        image::RgbImage::new(4096, 1024).save(&path).unwrap();
        let image = decode(&path).unwrap();
        assert_eq!((image.width(), image.height()), (2048, 512));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
