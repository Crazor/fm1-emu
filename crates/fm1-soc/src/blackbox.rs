//! The M-VAVE ANN Blackbox's panel, after Juice-Box's docs/06-stock-firmware-analysis.md (read
//! from the V20 firmware and confirmed by pressing each control on a pedal):
//!
//! * digital controls, pulled up (0 = pressed): FX PA0, HOME PA1, right footswitch PA2, left
//!   footswitch PA3, the PARA knob's click PH6, MOD PH8, AMP PH9;
//! * DLY, REV and CAB share a resistor ladder on PC10 (SARADC channel 9, 1023 at rest);
//! * the MASTER pot on PA10 (SARADC channel 2), the battery at half its voltage on PC9 (channel 8);
//! * the PARA knob is a quadrature encoder on PC0 / PC1, four transitions a detent: PC0 leads
//!   clockwise.

/// `Action::Key` ids on this board.
pub const FX: u8 = 0;
pub const HOME: u8 = 1;
pub const RIGHT: u8 = 2;
pub const LEFT: u8 = 3;
pub const AMP: u8 = 4;
pub const MOD: u8 = 5;
pub const CLICK: u8 = 6;
pub const DLY: u8 = 7;
pub const REV: u8 = 8;
pub const CAB: u8 = 9;

/// SARADC channels.
pub const ADC_MASTER: usize = 2;
pub const ADC_BATTERY: usize = 8;
pub const ADC_LADDER: u32 = 9;

/// The battery channel on a charged pedal (Juice-Box measured 645 on USB, charged for hours).
pub const BATTERY_FULL: u16 = 645;
/// Where the MASTER knob starts: full volume.
pub const MASTER_DEFAULT: u16 = 1023;

/// Port A's and port H's controls: (key, pin).
pub const PA: [(u8, u32); 4] = [(FX, 0), (HOME, 1), (RIGHT, 2), (LEFT, 3)];
pub const PH: [(u8, u32); 3] = [(CLICK, 6), (MOD, 8), (AMP, 9)];

/// A port's input word: every pin pulled up, the pressed controls low.
pub fn port_in(pins: &[(u8, u32)], keys: &[bool]) -> u32 {
    pins.iter().filter(|(key, _)| keys[*key as usize]).fold(0xffff, |word, (_, pin)| word & !(1 << pin))
}

/// The encoder's contacts on port C: `a` is PC1 and `b` PC0, closed = low.
pub fn encoder_in(a: u8, b: u8) -> u32 {
    0xffff & !(u32::from(a) << 1 | u32::from(b))
}

/// The resistor ladder's reading.
pub fn ladder(keys: &[bool]) -> u16 {
    [(CAB, 616), (DLY, 706), (REV, 787)].iter().find(|(key, _)| keys[*key as usize]).map_or(1023, |(_, v)| *v)
}

/// The panel's control names.
pub fn key_id(name: &str) -> Option<u8> {
    match name.to_ascii_uppercase().as_str() {
        "FX" => Some(FX),
        "HOME" => Some(HOME),
        "RIGHT" | "FSR" => Some(RIGHT),
        "LEFT" | "FSL" => Some(LEFT),
        "AMP" => Some(AMP),
        "MOD" => Some(MOD),
        "CLICK" => Some(CLICK),
        "DLY" => Some(DLY),
        "REV" => Some(REV),
        "CAB" => Some(CAB),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::periph::{Action, Periph};
    use crate::Board;

    fn panel() -> Periph {
        let mut p = Periph::default();
        p.board = Board::Blackbox;
        p
    }

    #[test]
    fn controls_read_high_until_pressed() {
        let mut p = panel();
        assert_eq!(p.read(0x50004), Some(0xffff));
        assert_eq!(p.read(0x501c4), Some(0xffff));
        p.apply(&Action::Key { id: LEFT, down: true });
        p.apply(&Action::Key { id: AMP, down: true });
        assert_eq!(p.read(0x50004), Some(0xffff & !(1 << 3)));
        assert_eq!(p.read(0x501c4), Some(0xffff & !(1 << 9)));
        p.apply(&Action::Key { id: LEFT, down: false });
        assert_eq!(p.read(0x50004), Some(0xffff));
    }

    #[test]
    fn the_ladder_rests_at_full_scale() {
        let mut keys = [false; 41];
        assert_eq!(ladder(&keys), 1023);
        keys[REV as usize] = true;
        assert_eq!(ladder(&keys), 787);
    }

    #[test]
    fn pc0_leads_clockwise() {
        let mut p = panel();
        assert_eq!(p.read(0x50084), Some(0xffff));
        p.apply(&Action::Enc { enc: 0, clicks: 1 });
        p.advance(1);
        assert_eq!(p.read(0x50084), Some(0xffff & !1), "PC0 closes first");
    }

    #[test]
    fn names_are_this_boards() {
        assert_eq!(key_id("cab"), Some(CAB));
        assert_eq!(key_id("PLAY"), None);
    }
}
