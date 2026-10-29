// SPDX-License-Identifier: AGPL-3.0-or-later

//! Client-side image rescaling for outbound prompts (CONTRACT §8.1).
//!
//! A phone camera photo rides the prompt as a multi-megabyte base64 data URI,
//! re-sent with every turn of history and cached per session in the core.
//! The policy that costs nothing visually but ~10x in bytes is one rule:
//! downscale so the long edge is at most [`MAX_EDGE`] pixels, and re-encode
//! JPEG at [`JPEG_QUALITY`] (PNG stays PNG so screenshots keep their format;
//! anything that decodes with meaningful transparency also stays PNG).
//!
//! This lives in the core, not the UIs: the policy is shared client behavior,
//! and the prompt path here transforms the very blocks that reach the wire,
//! the echoed row, and the transcript — one hook covers all three.
//!
//! Module invariant (INTERNAL.md): no network, no OS access. Pure bytes in,
//! bytes out; a decode failure passes the original through — compression must
//! never become "the message did not send".

/// Long-edge cap in pixels. Generous: vision models charge image tokens mostly
/// by resolution, and ~1600 px preserves readable text/OCR for screenshots.
/// Below ~1000 px labels start going soft, so this is the comfortable ceiling.
pub const MAX_EDGE: u32 = 1600;
/// JPEG re-encode quality (0-100). Above ~85 the bytes climb faster than the
/// visually-presentable quality does.
pub const JPEG_QUALITY: u32 = 80;

/// The rescale decision for one image block.
#[derive(Debug, PartialEq)]
pub struct Rescaled {
    pub mime_type: String,
    /// Base64 (as the prompt carried it — NO-wrap not required; the SDK and
    /// every decoder here accept embedded padding).
    pub data: String,
}

/// Rescale one base64-encoded image if it exceeds [`MAX_EDGE`] on its long
/// edge. `Ok(None)` = unchanged (small enough, unsupported format, or a decode
/// failure) — the caller keeps the original bytes.
pub fn rescale(mime_type: &str, data: &str) -> Result<Option<Rescaled>, String> {
    let bytes = base64_decode(data).ok_or_else(|| "not base64".to_string())?;
    // WebP and every unhandled codec pass through: JPEG covers phone cameras,
    // PNG covers screenshots, and an unknown format's bytes are better sent
    // than dropped (the core alerts instead).
    match image::guess_format(&bytes) {
        Ok(image::ImageFormat::Jpeg) | Ok(image::ImageFormat::Png) => {}
        _ => return Ok(None),
    }
    let decoded = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    let (w, h) = (decoded.width(), decoded.height());
    let long = w.max(h);
    if long <= MAX_EDGE {
        return Ok(None);
    }
    let scale = MAX_EDGE as f64 / long as f64;
    let (nw, nh) = (
        ((w as f64) * scale).round().max(1.0) as u32,
        ((h as f64) * scale).round().max(1.0) as u32,
    );
    // The geometry is exact (nw/nh preserve the aspect ratio), so resize_exact;
    // Triangle is fast and, at 10x reductions of a camera photo, visually
    // indistinguishable from Lanczos here.
    let resized = decoded.resize_exact(nw, nh, image::imageops::FilterType::Triangle);
    // Transparency below the JPEG format (screenshots with alpha) stays PNG;
    // an all-opaque PNG costs the same tokens as a JPEG at any size.
    let has_alpha = decoded.color().has_alpha();
    if mime_type == "image/jpeg" && !has_alpha {
        let mut out = Vec::new();
        image::ImageEncoder::write_image(
            image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut out,
                JPEG_QUALITY as u8,
            ),
            resized.to_rgb8().as_raw(),
            nw,
            nh,
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| e.to_string())?;
        let data = base64_encode(&out);
        return Ok(Some(Rescaled {
            mime_type: "image/jpeg".to_string(),
            data,
        }));
    }
    // PNG source (or alpha): re-encode at the smaller geometry. PNG is lossless
    // and slower to shrink, but it is what preserves the transparency.
    let mut out = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(&mut out),
        resized.to_rgba8().as_raw(),
        nw,
        nh,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|e| e.to_string())?;
    Ok(Some(Rescaled {
        mime_type: "image/png".to_string(),
        data: base64_encode(&out),
    }))
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(s.trim()).ok()
}

fn base64_encode(b: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::GenericImageView;
    use image::{DynamicImage, Rgba, RgbaImage};

    fn jpeg_b64(w: u32, h: u32, fill: [u8; 3]) -> String {
        let img = image::RgbImage::from_fn(w, h, |_, _| image::Rgb(fill));
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut out)
            .encode_image(&DynamicImage::ImageRgb8(img))
            .unwrap();
        base64_encode(&out)
    }

    fn png_b64(w: u32, h: u32) -> (String, bool) {
        let mut img = RgbaImage::new(w, h);
        img.put_pixel(0, 0, Rgba([255, 0, 0, 128])); // alpha present
        let mut out = Vec::new();
        image::ImageEncoder::write_image(
            image::codecs::png::PngEncoder::new(&mut out),
            img.as_raw(),
            w,
            h,
            image::ExtendedColorType::Rgba8,
        )
            .unwrap();
        (base64_encode(&out), true)
    }

    #[test]
    fn small_image_is_unchanged() {
        assert_eq!(rescale("image/jpeg", &jpeg_b64(300, 200, [10, 20, 30])).unwrap(), None);
    }

    #[test]
    fn large_jpeg_shrinks_to_edge_and_jpeg() {
        let r = rescale("image/jpeg", &jpeg_b64(4000, 3000, [10, 20, 30]))
            .unwrap()
            .expect("expected a rescale");
        assert_eq!(r.mime_type, "image/jpeg");
        let decoded = image::load_from_memory(&base64_decode(&r.data).unwrap()).unwrap();
        let (w, h) = (decoded.width(), decoded.height());
        assert_eq!(w.max(h), MAX_EDGE);
        assert!((1600, 1200) == (w, h), "expected 1600x1200, got {w}x{h}");
        // The re-encode is a genuine byte reduction on a big photo.
        assert!(r.data.len() < jpeg_b64(4000, 3000, [10, 20, 30]).len());
    }

    #[test]
    fn large_png_resizes_and_keeps_png_for_alpha() {
        let (src, _) = png_b64(4000, 3000);
        let r = rescale("image/png", &src).unwrap().expect("rescale");
        assert_eq!(r.mime_type, "image/png");
        assert_eq!(
            image::load_from_memory(&base64_decode(&r.data).unwrap()).unwrap().dimensions(),
            (1600, 1200)
        );
    }

    #[test]
    fn unsupported_format_passes_through() {
        // A valid WebP header is enough to trip the format gate.
        let bytes = vec![b'R', b'I', b'F', b'F', 0, 0, 0, 0, b'W', b'E', b'B', b'P'];
        assert_eq!(rescale("image/webp", &base64_encode(&bytes)).unwrap(), None);
    }

    #[test]
    fn corrupt_base64_is_an_error_not_a_crash() {
        assert!(rescale("image/jpeg", "!!!not base64!!!").is_err());
        // Valid base64 but not an image: decode failure → passthrough.
        assert_eq!(rescale("image/jpeg", &base64_encode(b"plain text")).unwrap(), None);
    }

    #[test]
    fn shorter_edge_rounds_to_at_least_one_pixel() {
        let r = rescale("image/jpeg", &jpeg_b64(20000, 3000, [10, 20, 30]))
            .unwrap()
            .expect("rescale");
        let decoded = image::load_from_memory(&base64_decode(&r.data).unwrap()).unwrap();
        let (w, h) = decoded.dimensions();
        assert_eq!((w, h), (1600, 240));
    }
}
