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

/// Each button's LED in the shift register, in the panel's order (HOME, FX, AMP, MOD, DLY, REV, CAB).
pub const LED_BIT: [u8; 7] = [0, 1, 2, 3, 4, 5, 6];

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

/// A WAV file's first channel as 24-bit samples for the guitar input: PCM of 16, 24 or 32 bits or
/// 32-bit float, at 44.1 kHz. (The codec runs at 44118 Hz; the samples are played one for one.)
pub fn wav_input(file: &[u8]) -> Result<Vec<i32>, String> {
    if file.len() < 12 || &file[..4] != b"RIFF" || &file[8..12] != b"WAVE" {
        return Err("not a WAV file".into());
    }
    let u16_at = |b: &[u8], i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let mut format = None;
    let mut at = 12;
    while at + 8 <= file.len() {
        let size = u32::from_le_bytes([file[at + 4], file[at + 5], file[at + 6], file[at + 7]]) as usize;
        let body = at + 8;
        let end = body.saturating_add(size).min(file.len());
        match &file[at..at + 4] {
            b"fmt " if end - body >= 16 => {
                let f = &file[body..end];
                let tag = u16_at(f, 0);
                // WAVE_FORMAT_EXTENSIBLE keeps the real tag at the start of its subformat
                let tag = if tag == 0xfffe && f.len() >= 26 { u16_at(f, 24) } else { tag };
                let rate = u32::from_le_bytes([f[4], f[5], f[6], f[7]]);
                format = Some((tag, u16_at(f, 2) as usize, rate, u16_at(f, 14) as usize));
            }
            b"data" => {
                let (tag, channels, rate, bits) = format.ok_or("no fmt chunk before the data")?;
                if rate != 44100 {
                    return Err(format!("{rate} Hz: the input must be 44100 Hz"));
                }
                if channels == 0 {
                    return Err("no channels".into());
                }
                let sample = |b: &[u8]| -> Option<i32> {
                    Some(match (tag, bits) {
                        (1, 16) => i32::from(i16::from_le_bytes([b[0], b[1]])) << 8,
                        (1, 24) => i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8,
                        (1, 32) => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) >> 8,
                        (3, 32) => (f32::from_le_bytes([b[0], b[1], b[2], b[3]]).clamp(-1.0, 1.0) * 8_388_607.0) as i32,
                        _ => return None,
                    })
                };
                let step = channels * bits / 8;
                if step == 0 {
                    return Err("empty frames".into());
                }
                return file[body..end]
                    .chunks_exact(step)
                    .map(|frame| sample(frame).ok_or_else(|| format!("unsupported WAV format {tag}, {bits} bits")))
                    .collect();
            }
            _ => {}
        }
        at = body.saturating_add(size).saturating_add(size & 1);
    }
    Err("no data chunk".into())
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

    fn wav(rate: u32, channels: u16, bits: u16, data: &[u8]) -> Vec<u8> {
        let mut f = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0".to_vec();
        f.extend(channels.to_le_bytes());
        f.extend(rate.to_le_bytes());
        f.extend((rate * u32::from(channels) * u32::from(bits) / 8).to_le_bytes());
        f.extend((channels * bits / 8).to_le_bytes());
        f.extend(bits.to_le_bytes());
        f.extend(b"data");
        f.extend((data.len() as u32).to_le_bytes());
        f.extend(data);
        f
    }

    #[test]
    fn wav_input_takes_the_first_channel_at_24_bits() {
        let stereo_16 = [0x00, 0x40, 0x11, 0x11, 0x00, 0xc0, 0x22, 0x22];
        assert_eq!(wav_input(&wav(44100, 2, 16, &stereo_16)), Ok(vec![0x400000, -0x400000]));
        let mono_24 = [0x56, 0x34, 0x12, 0xff, 0xff, 0xff];
        assert_eq!(wav_input(&wav(44100, 1, 24, &mono_24)), Ok(vec![0x123456, -1]));
    }

    #[test]
    fn wav_input_refuses_what_it_cannot_play() {
        assert!(wav_input(&wav(48000, 1, 16, &[0, 0])).unwrap_err().contains("48000"));
        assert!(wav_input(&wav(44100, 1, 8, &[0])).unwrap_err().contains("unsupported"));
        assert!(wav_input(b"not a wav").is_err());
    }

    #[test]
    fn each_channel_in_use_shows_its_half() {
        let mut p = panel();
        p.write(0x12e20, 128);
        p.write(0x12e04, 0xd5); // channel 0 sends, channel 1 receives
        p.write(0x12e00, 0x800);
        assert_eq!(p.read(0x12e00).unwrap() & 0xf000, 0);
        p.advance(400_000);
        assert_eq!(p.read(0x12e00).unwrap() & 0xf000, 0x3000, "bits 12 and 13, not 15");
        p.advance(400_000);
        assert_eq!(p.read(0x12e00).unwrap() & 0xf000, 0);
    }

    #[test]
    fn a_latched_zero_lights_its_led() {
        const CLOCK: u32 = 1 << 5;
        const LATCH: u32 = 1 << 6;
        const DATA: u32 = 1 << 8;
        let mut p = panel();
        // all ones except bit 2 (the first bit shifted in ends up as bit 7)
        for bit in (0..8).rev() {
            let data = if bit == 2 { 0 } else { DATA };
            p.write(0x50080, data);
            p.write(0x50080, data | CLOCK);
        }
        p.write(0x50080, LATCH);
        p.write(0x50080, 0);
        let _ = p.led_take();
        p.advance(1000);
        let leds = p.led_take();
        assert_eq!(leds[LED_BIT[2] as usize], 1.0, "AMP's LED");
        assert_eq!(leds[LED_BIT[1] as usize], 0.0, "FX's LED");
    }

    #[test]
    fn names_are_this_boards() {
        assert_eq!(key_id("cab"), Some(CAB));
        assert_eq!(key_id("PLAY"), None);
    }
}
