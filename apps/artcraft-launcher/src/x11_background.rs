//! Linux and the BSDs on X11: the window's background colour.
//!
//! winit creates its X11 windows without a background, so while the window grows the area it gains
//! keeps whatever was in that memory (earlier frames, other windows) until the next frame is drawn:
//! trails behind the edge being dragged (issue #69). With the launcher's own background set on the
//! window, the X server fills that area with it instead, which reads as the window's empty page.
//! Wayland compositors wait for the app's next frame instead, so this module only acts on X11.

use std::error::Error;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ChangeWindowAttributesAux, ConnectionExt as _};

/// Sets X11 window `window`'s background to `rgb`. Failing only means the trails stay, so it logs
/// and carries on.
pub fn set(window: u32, rgb: [u8; 3]) {
    if let Err(e) = try_set(window, rgb) {
        log::debug!("couldn't set the window's X11 background: {e}");
    }
}

fn try_set(window: u32, rgb: [u8; 3]) -> Result<(), Box<dyn Error>> {
    let (conn, _) = x11rb::connect(None)?;
    let attributes = conn.get_window_attributes(window)?.reply()?;
    let depth = conn.get_geometry(window)?.reply()?.depth;
    let visual = conn
        .setup()
        .roots
        .iter()
        .flat_map(|screen| &screen.allowed_depths)
        .flat_map(|d| &d.visuals)
        .find(|v| v.visual_id == attributes.visual)
        .ok_or("the window's visual isn't in the server's list")?;
    let pixel = pixel(rgb, [visual.red_mask, visual.green_mask, visual.blue_mask], depth);
    conn.change_window_attributes(window, &ChangeWindowAttributesAux::new().background_pixel(pixel))?.check()?;
    Ok(())
}

/// `rgb` as a pixel of a TrueColor visual with these channel masks. A 32-bit visual keeps alpha in
/// the bits no colour uses: those are set, so the background is opaque.
fn pixel(rgb: [u8; 3], masks: [u32; 3], depth: u8) -> u32 {
    let mut pixel = 0;
    for (value, mask) in rgb.into_iter().zip(masks) {
        if mask == 0 {
            continue;
        }
        let bits = mask.count_ones();
        let scaled = if bits >= 8 { u32::from(value) << (bits - 8) } else { u32::from(value) >> (8 - bits) };
        pixel |= (scaled << mask.trailing_zeros()) & mask;
    }
    if depth == 32 {
        pixel |= !(masks[0] | masks[1] | masks[2]);
    }
    pixel
}

#[cfg(test)]
mod tests {
    use super::pixel;

    const RGB888: [u32; 3] = [0xff_0000, 0x00_ff00, 0x00_00ff];

    #[test]
    fn a_24_bit_window_takes_the_colour_as_is() {
        assert_eq!(pixel([0x12, 0x13, 0x16], RGB888, 24), 0x12_1316);
        assert_eq!(pixel([0xf2, 0xf1, 0xee], RGB888, 24), 0xf2_f1ee);
    }

    #[test]
    fn a_32_bit_window_gets_an_opaque_background() {
        assert_eq!(pixel([0x12, 0x13, 0x16], RGB888, 32), 0xff12_1316);
    }

    #[test]
    fn a_16_bit_window_keeps_each_channels_top_bits() {
        let rgb565 = [0xf800, 0x07e0, 0x001f];
        assert_eq!(pixel([0xff, 0xff, 0xff], rgb565, 16), 0xffff);
        assert_eq!(pixel([0x12, 0x13, 0x16], rgb565, 16), (0x02 << 11) | (0x04 << 5) | 0x02);
    }
}
