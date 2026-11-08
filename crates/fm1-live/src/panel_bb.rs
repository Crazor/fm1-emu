//! The ANN Blackbox's front panel, drawn after product photos of the pedal and at its proportions:
//! a matte black body, MASTER and PARA either side of a small screen, seven white backlit buttons
//! in a row, and two chrome footswitches joined by the TUNER line. Maker's logos are left off.
//! Ids are fm1_soc::blackbox's.

use crate::panel::{canvas, disc, mix, put, rrect, text, ROOM};
use fm1_soc::blackbox as bb;

pub const W: usize = 720;
pub const H: usize = 760;
pub const LCD_X: usize = 240;
pub const LCD_Y: usize = 70;

const BODY_EDGE: u32 = 0x34363a;
const BODY_TOP: u32 = 0x1b1c1f;
const BODY_BOT: u32 = 0x121315;
const INK: u32 = 0xa9abaf;
const INK_DIM: u32 = 0x74767b;
const KEY_LIT: u32 = 0xf1f2ee;
const KEY_SHADE: u32 = 0xc9cbc8;
const KEY_TEXT: u32 = 0x2c2d31;
/// A button with its LED off.
const KEY_DARK: u32 = 0x8f9190;
const KEY_DARK_LOW: u32 = 0x777978;
/// Where a pressed control's light goes.
const PRESSED: u32 = 0x9fc4ff;

const MASTER: (f32, f32) = (120.0, 170.0);
const PARA: (f32, f32) = (600.0, 170.0);
const KNOB_R: f32 = 60.0;
const FOOT_Y: f32 = 590.0;
const FOOT_R: f32 = 62.0;
const FEET: [(u8, f32); 2] = [(bb::LEFT, 120.0), (bb::RIGHT, 600.0)];
const BUTTON_Y: usize = 390;
const BUTTON_W: usize = 60;
const BUTTON_H: usize = 58;

/// The buttons from the left, as the keyboard's 1..7 press them.
pub const BUTTONS: [(&str, u8); 7] =
    [("HOME", bb::HOME), ("FX", bb::FX), ("AMP", bb::AMP), ("MOD", bb::MOD), ("DLY", bb::DLY), ("REV", bb::REV), ("CAB", bb::CAB)];

fn button_x(i: usize) -> usize {
    78 + i * 84
}

/// What a point on the panel is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ctl {
    Master,
    Para,
    Btn(u8),
    /// The TUNER line: both footswitches together.
    Tuner,
}

pub fn hit(x: f32, y: f32) -> Option<Ctl> {
    let near = |(cx, cy): (f32, f32), r: f32| (x - cx).hypot(y - cy) <= r;
    if near(MASTER, KNOB_R + 6.0) {
        return Some(Ctl::Master);
    }
    if near(PARA, KNOB_R + 6.0) {
        return Some(Ctl::Para);
    }
    if let Some((id, _)) = FEET.iter().find(|(_, cx)| near((*cx, FOOT_Y), FOOT_R)) {
        return Some(Ctl::Btn(*id));
    }
    if (FEET[0].1 + FOOT_R..FEET[1].1 - FOOT_R).contains(&x) && (y - FOOT_Y).abs() <= 18.0 {
        return Some(Ctl::Tuner);
    }
    let inside_row = (BUTTON_Y as f32..(BUTTON_Y + BUTTON_H) as f32).contains(&y);
    BUTTONS.iter().enumerate().find_map(|(i, (_, id))| {
        let left = button_x(i) as f32;
        (inside_row && (left..left + BUTTON_W as f32).contains(&x)).then_some(Ctl::Btn(*id))
    })
}

fn screw(buf: &mut [u32], cx: f32, cy: f32) {
    disc(buf, cx, cy, 9.0, |_, dy| mix(0xb4b6ba, 0x55575c, dy / 18.0 + 0.5));
    for t in -5..=5 {
        put(buf, cx as i32 + t, cy as i32 + t, 0x26272a, 1.0);
        put(buf, cx as i32 + t, cy as i32 - t, 0x26272a, 1.0);
    }
}

/// A small triangle pointing up (`dir` -1) or down (1), centred on `cx`.
fn arrow(buf: &mut [u32], cx: i32, cy: i32, dir: i32, color: u32) {
    for row in 0..7 {
        let half = if dir > 0 { 6 - row } else { row };
        for x in -half..=half {
            put(buf, cx + x, cy + row, color, 1.0);
        }
    }
}

pub fn base() -> Vec<u32> {
    canvas(W);
    let mut b = vec![ROOM; W * H];
    rrect(&mut b, 8.0, 8.0, (W - 16) as f32, (H - 16) as f32, 20.0, BODY_EDGE, 0x202124);
    rrect(&mut b, 11.0, 11.0, (W - 22) as f32, (H - 22) as f32, 18.0, BODY_TOP, BODY_BOT);
    screw(&mut b, 34.0, 36.0);
    screw(&mut b, (W - 34) as f32, 36.0);
    // the screen sits in a raised gloss-black window
    rrect(&mut b, (LCD_X - 22) as f32, (LCD_Y - 22) as f32, 284.0, 284.0, 16.0, 0x2f3034, 0x1f2023);
    rrect(&mut b, (LCD_X - 20) as f32, (LCD_Y - 20) as f32, 280.0, 280.0, 15.0, 0x040405, 0x08080a);
    text(&mut b, MASTER.0, MASTER.1 + KNOB_R + 16.0, "MASTER", 2, INK);
    text(&mut b, PARA.0, PARA.1 + KNOB_R + 16.0, "PARA", 2, INK);
    // the TUNER line between the footswitches
    let (left, right) = (FEET[0].1 + FOOT_R + 14.0, FEET[1].1 - FOOT_R - 14.0);
    text(&mut b, W as f32 / 2.0, FOOT_Y - 7.0, "TUNER", 2, INK);
    for (y, gap) in [(FOOT_Y - 3.0, 44.0), (FOOT_Y + 2.0, 44.0)] {
        rrect(&mut b, left, y, W as f32 / 2.0 - gap - left, 1.5, 0.5, INK_DIM, INK_DIM);
        rrect(&mut b, W as f32 / 2.0 + gap, y, right - W as f32 / 2.0 - gap, 1.5, 0.5, INK_DIM, INK_DIM);
    }
    arrow(&mut b, FEET[0].1 as i32, FOOT_Y as i32 + FOOT_R as i32 + 18, 1, INK);
    arrow(&mut b, FEET[1].1 as i32, FOOT_Y as i32 + FOOT_R as i32 + 18, -1, INK);
    b
}

pub struct State<'a> {
    pub lcd: &'a [u16],
    pub master: u16,
    pub para_clicks: i32,
    /// Held controls, by id.
    pub down: &'a [bool],
    /// How much of the time each button's LED has been lit, by shift-register bit.
    pub leds: &'a [f32],
}

fn knob(buf: &mut [u32], (cx, cy): (f32, f32), angle: f32) {
    disc(buf, cx + 2.0, cy + 6.0, KNOB_R + 3.0, |_, _| 0x08080a);
    // the knurled skirt, then the smooth cap with light from above
    disc(buf, cx, cy, KNOB_R, |dx, dy| {
        let ridge = (dy.atan2(dx) * 48.0).sin() * 0.5 + 0.5;
        mix(0x101113, 0x3a3b3f, ridge * 0.7 - dy / KNOB_R * 0.25)
    });
    disc(buf, cx, cy, KNOB_R * 0.78, |dx, dy| {
        let light = (-(dx * 0.3 + dy) / KNOB_R).clamp(-1.0, 1.0) * 0.5 + 0.5;
        mix(0x141517, 0x34353a, light)
    });
    let (s, c) = angle.sin_cos();
    let d = KNOB_R * 0.62;
    disc(buf, cx + s * d, cy - c * d, 3.2, |_, _| 0xf0f0ec);
}

fn footswitch(buf: &mut [u32], cx: f32, held: bool) {
    let cy = FOOT_Y;
    disc(buf, cx + 2.0, cy + 6.0, FOOT_R + 2.0, |_, _| 0x08080a);
    // the chrome ring and nut
    disc(buf, cx, cy, FOOT_R, |dx, dy| {
        let sheen = ((dy.atan2(dx) * 2.0 + 0.8).cos() * 0.5 + 0.5).powf(1.6);
        mix(0x6d7076, 0xf0f1f2, sheen)
    });
    disc(buf, cx, cy, FOOT_R * 0.82, |_, dy| mix(0xb9bbc0, 0x7a7c82, dy / FOOT_R + 0.5));
    // the plunger, which moves in when pressed
    let cap = if held { FOOT_R * 0.60 } else { FOOT_R * 0.66 };
    disc(buf, cx, cy, cap, |dx, dy| {
        let light = (-(dx * 0.4 + dy) / cap).clamp(-1.0, 1.0) * 0.5 + 0.5;
        let (dark, bright) = if held { (0x7e8086, PRESSED) } else { (0x9fa2a8, 0xf7f8f8) };
        mix(dark, bright, light)
    });
}

pub fn draw(buf: &mut [u32], base: &[u32], st: &State) {
    canvas(W);
    buf.copy_from_slice(base);
    for y in 0..240 {
        for x in 0..240 {
            let p = st.lcd[y * 240 + x] as u32;
            buf[(LCD_Y + y) * W + LCD_X + x] = ((p >> 11) * 255 / 31) << 16 | (((p >> 5) & 63) * 255 / 63) << 8 | (p & 31) * 255 / 31;
        }
    }
    // MASTER sweeps 300 degrees, PARA turns freely, 24 detents a turn
    let sweep = f32::from(st.master) / 1023.0 * 300.0 - 150.0;
    knob(buf, MASTER, sweep.to_radians());
    knob(buf, PARA, (st.para_clicks as f32 * 15.0).to_radians());
    let held = |id: u8| st.down.get(id as usize).copied().unwrap_or(false);
    for (i, (name, id)) in BUTTONS.iter().enumerate() {
        let (x, y, pressed) = (button_x(i) as f32, BUTTON_Y as f32, held(*id));
        if pressed {
            rrect(buf, x - 3.0, y - 3.0, BUTTON_W as f32 + 6.0, BUTTON_H as f32 + 6.0, 13.0, PRESSED, PRESSED);
        } else {
            rrect(buf, x + 1.0, y + 4.0, BUTTON_W as f32, BUTTON_H as f32, 10.0, 0x08080a, 0x08080a);
        }
        let bit = bb::LED_BIT[i] as usize;
        let lit = st.leds.get(bit).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        let (top, bot) = if pressed {
            (KEY_SHADE, 0xa8abaa)
        } else {
            (mix(KEY_DARK, KEY_LIT, lit), mix(KEY_DARK_LOW, KEY_SHADE, lit))
        };
        if lit > 0.0 && !pressed {
            rrect(buf, x - 3.0, y - 3.0, BUTTON_W as f32 + 6.0, BUTTON_H as f32 + 6.0, 13.0, mix(BODY_TOP, KEY_LIT, lit * 0.35), mix(BODY_TOP, KEY_LIT, lit * 0.35));
        }
        rrect(buf, x, y, BUTTON_W as f32, BUTTON_H as f32, 10.0, top, bot);
        text(buf, x + BUTTON_W as f32 / 2.0, y + 22.0, name, 2, KEY_TEXT);
    }
    for (id, cx) in FEET {
        footswitch(buf, cx, held(id));
    }
    if held(bb::LEFT) && held(bb::RIGHT) {
        text(buf, W as f32 / 2.0, FOOT_Y - 7.0, "TUNER", 2, PRESSED);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_control_can_be_hit() {
        assert_eq!(hit(MASTER.0, MASTER.1), Some(Ctl::Master));
        assert_eq!(hit(PARA.0, PARA.1), Some(Ctl::Para));
        for (i, (_, id)) in BUTTONS.iter().enumerate() {
            assert_eq!(hit(button_x(i) as f32 + 10.0, BUTTON_Y as f32 + 10.0), Some(Ctl::Btn(*id)));
        }
        for (id, x) in FEET {
            assert_eq!(hit(x, FOOT_Y), Some(Ctl::Btn(id)));
        }
        assert_eq!(hit(W as f32 / 2.0, FOOT_Y), Some(Ctl::Tuner));
        assert_eq!(hit(360.0, 190.0), None, "the screen is not a control");
    }

    #[test]
    fn the_panel_fits_its_window() {
        let right = button_x(BUTTONS.len() - 1) + BUTTON_W;
        assert!(right < W - 11);
        assert!(FOOT_Y + FOOT_R + 25.0 < (H - 11) as f32);
    }
}
