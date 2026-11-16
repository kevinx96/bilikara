//! Bounded local image decoding. No image or decoded credential is persisted.
pub fn decode_qr_luma(width: usize, height: usize, pixels: &[u8]) -> Result<String, &'static str> {
    if width == 0
        || height == 0
        || width > 2048
        || height > 2048
        || width.checked_mul(height) != Some(pixels.len())
    {
        return Err("cast_capture_invalid");
    }
    let mut decoder = quircs::Quirc::default();
    let mut found = None;
    for code in decoder.identify(width, height, pixels).flatten() {
        let Ok(decoded) = code.decode() else { continue };
        let Ok(value) = String::from_utf8(decoded.payload) else {
            continue;
        };
        if value.len() > 32768 || value.chars().any(char::is_control) {
            continue;
        }
        if found.as_ref().is_some_and(|previous| previous != &value) {
            return Err("cast_capture_ambiguous");
        }
        found = Some(value);
    }
    found.ok_or("cast_capture_not_found")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_decode_roundtrip_and_empty_image() {
        let text = "https://wx.chunk.cn/projection/login?test=local-only";
        let code = qrcode::QrCode::new(text).unwrap();
        let side = (code.width() + 8) * 6;
        let mut pixels = vec![255; side * side];
        for y in 0..code.width() {
            for x in 0..code.width() {
                if code[(x, y)] == qrcode::Color::Dark {
                    for dy in 0..6 {
                        for dx in 0..6 {
                            pixels[((y + 4) * 6 + dy) * side + (x + 4) * 6 + dx] = 0;
                        }
                    }
                }
            }
        }
        assert_eq!(decode_qr_luma(side, side, &pixels).unwrap(), text);
        assert_eq!(
            decode_qr_luma(10, 10, &[255; 100]),
            Err("cast_capture_not_found")
        );
        assert_eq!(decode_qr_luma(2049, 1, &[0]), Err("cast_capture_invalid"));
        assert_eq!(decode_qr_luma(1, 2, &[0]), Err("cast_capture_invalid"));
    }
}
