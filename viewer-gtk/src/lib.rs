use anyhow::{ensure, Context, Result};

pub fn rgb565_to_rgba(width: u32, height: u32, stride: usize, bytes: &[u8]) -> Result<Vec<u8>> {
    let width = width as usize;
    let height = height as usize;
    let min_len = stride
        .checked_mul(height)
        .context("shared framebuffer length overflow")?;
    ensure!(
        bytes.len() >= min_len,
        "shared framebuffer too short: got {} bytes, need at least {}",
        bytes.len(),
        min_len
    );

    let mut rgba = vec![0_u8; width * height * 4];
    for y in 0..height {
        let src_row = &bytes[y * stride..y * stride + width * 2];
        let dst_row = &mut rgba[y * width * 4..(y + 1) * width * 4];
        for x in 0..width {
            let src_idx = x * 2;
            let pixel = u16::from_le_bytes([src_row[src_idx], src_row[src_idx + 1]]);
            let red = ((pixel >> 11) & 0x1f) as u8;
            let green = ((pixel >> 5) & 0x3f) as u8;
            let blue = (pixel & 0x1f) as u8;
            let dst_idx = x * 4;
            dst_row[dst_idx] = (red << 3) | (red >> 2);
            dst_row[dst_idx + 1] = (green << 2) | (green >> 4);
            dst_row[dst_idx + 2] = (blue << 3) | (blue >> 2);
            dst_row[dst_idx + 3] = 0xff;
        }
    }

    Ok(rgba)
}

#[cfg(test)]
mod tests {
    use super::rgb565_to_rgba;

    #[test]
    fn converts_rgb565_to_rgba() {
        let pixels = [
            0x00, 0xf8, // red
            0xe0, 0x07, // green
            0x1f, 0x00, // blue
            0xff, 0xff, // white
        ];

        let rgba = rgb565_to_rgba(2, 2, 4, &pixels).expect("convert framebuffer");
        assert_eq!(
            rgba,
            vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,]
        );
    }
}
