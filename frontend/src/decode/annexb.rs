//! The little bit of Annex B parsing the decoder needs: whether an access unit
//! carries a keyframe, and what codec string the stream's own SPS implies.

/// Walk Annex B start codes, calling `f` with each NAL unit's first byte.
fn nal_units(payload: &[u8], mut f: impl FnMut(&[u8]) -> bool) {
    let mut i = 0;
    while i + 3 < payload.len() {
        // Start codes are 00 00 01 or 00 00 00 01; both end with 00 00 01.
        if payload[i] == 0 && payload[i + 1] == 0 && payload[i + 2] == 1 {
            let nal = &payload[i + 3..];
            if !nal.is_empty() && f(nal) {
                return;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
}

/// Does this access unit contain an IDR (NAL type 5)?
pub fn is_keyframe(payload: &[u8]) -> bool {
    let mut idr = false;
    nal_units(payload, |nal| {
        if nal[0] & 0x1F == 5 {
            idr = true;
            return true;
        }
        false
    });
    idr
}

/// Build the `avc1.PPCCLL` codec string out of the stream's own SPS.
///
/// Hardcoding this would be a guess: the encoder picks a level from the
/// resolution and bitrate it was given, so a 1280x800 surface and a 1080p one do
/// not agree, and a codec string below the real level can be rejected outright.
pub fn codec_string(payload: &[u8]) -> Option<String> {
    let mut out = None;
    nal_units(payload, |nal| {
        // SPS is type 7; profile_idc, constraint flags and level_idc are the
        // three bytes straight after the NAL header.
        if nal[0] & 0x1F == 7 && nal.len() >= 4 {
            out = Some(format!("avc1.{:02X}{:02X}{:02X}", nal[1], nal[2], nal[3]));
            return true;
        }
        false
    });
    out
}
