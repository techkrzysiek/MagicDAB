//! DAB character sets (ETSI TS 101 756): EBU Latin based repertoire,
//! UCS-2 and UTF-8.

/// 0x0A preferred line break and 0x0B end of headline are kept as line breaks,
/// 0x1F preferred word break as a soft hyphen: DL Plus tags count characters
/// of the message with them (EN 300 401 7.4.5.2, TS 102 980).
const EBU_00_1F: [char; 32] = [
    '\0', '\u{0118}', '\u{012E}', '\u{0172}', '\u{0102}', '\u{0116}', '\u{010E}', '\u{0218}', '\u{021A}', '\u{010A}', '\n', '\n',
    '\u{0120}', '\u{0139}', '\u{017B}', '\u{0143}', '\u{0105}', '\u{0119}', '\u{012F}', '\u{0173}', '\u{0103}', '\u{0117}', '\u{010F}',
    '\u{0219}', '\u{021B}', '\u{010B}', '\u{0147}', '\u{011A}', '\u{0121}', '\u{013A}', '\u{017C}', '\u{00AD}',
];

const EBU_7B_FF: [char; 133] = [
    '\u{00AB}', '\u{016F}', '\u{00BB}', '\u{013D}', '\u{0126}', '\u{00E1}', '\u{00E0}', '\u{00E9}', '\u{00E8}', '\u{00ED}', '\u{00EC}',
    '\u{00F3}', '\u{00F2}', '\u{00FA}', '\u{00F9}', '\u{00D1}', '\u{00C7}', '\u{015E}', '\u{00DF}', '\u{00A1}', '\u{0178}', '\u{00E2}',
    '\u{00E4}', '\u{00EA}', '\u{00EB}', '\u{00EE}', '\u{00EF}', '\u{00F4}', '\u{00F6}', '\u{00FB}', '\u{00FC}', '\u{00F1}', '\u{00E7}',
    '\u{015F}', '\u{011F}', '\u{0131}', '\u{00FF}', '\u{0136}', '\u{0145}', '\u{00A9}', '\u{0122}', '\u{011E}', '\u{011B}', '\u{0148}',
    '\u{0151}', '\u{0150}', '\u{20AC}', '\u{00A3}', '\u{0024}', '\u{0100}', '\u{0112}', '\u{012A}', '\u{016A}', '\u{0137}', '\u{0146}',
    '\u{013B}', '\u{0123}', '\u{013C}', '\u{0130}', '\u{0144}', '\u{0171}', '\u{0170}', '\u{00BF}', '\u{013E}', '\u{00B0}', '\u{0101}',
    '\u{0113}', '\u{012B}', '\u{016B}', '\u{00C1}', '\u{00C0}', '\u{00C9}', '\u{00C8}', '\u{00CD}', '\u{00CC}', '\u{00D3}', '\u{00D2}',
    '\u{00DA}', '\u{00D9}', '\u{0158}', '\u{010C}', '\u{0160}', '\u{017D}', '\u{00D0}', '\u{013F}', '\u{00C2}', '\u{00C4}', '\u{00CA}',
    '\u{00CB}', '\u{00CE}', '\u{00CF}', '\u{00D4}', '\u{00D6}', '\u{00DB}', '\u{00DC}', '\u{0159}', '\u{010D}', '\u{0161}', '\u{017E}',
    '\u{0111}', '\u{0140}', '\u{00C3}', '\u{00C5}', '\u{00C6}', '\u{0152}', '\u{0177}', '\u{00DD}', '\u{00D5}', '\u{00D8}', '\u{00DE}',
    '\u{014A}', '\u{0154}', '\u{0106}', '\u{015A}', '\u{0179}', '\u{0164}', '\u{00F0}', '\u{00E3}', '\u{00E5}', '\u{00E6}', '\u{0153}',
    '\u{0175}', '\u{00FD}', '\u{00F5}', '\u{00F8}', '\u{00FE}', '\u{014B}', '\u{0155}', '\u{0107}', '\u{015B}', '\u{017A}', '\u{0165}',
    '\u{0127}',
];

pub fn ebu_char(b: u8) -> char {
    match b {
        0x00..=0x1F => EBU_00_1F[b as usize],
        0x24 => '\u{0142}',
        0x5C => '\u{016E}',
        0x5E => '\u{0141}',
        0x60 => '\u{0104}',
        0x7B..=0xFF => EBU_7B_FF[(b - 0x7B) as usize],
        _ => b as char,
    }
}

/// Convert a DAB string with charset id (0 EBU Latin, 4 ISO 8859-1, 6 UCS-2 BE, 15 UTF-8).
pub fn decode(data: &[u8], charset: u8) -> String {
    match charset {
        15 => String::from_utf8_lossy(data).trim_end_matches('\0').to_string(),
        6 => {
            let units: Vec<u16> = data.chunks_exact(2).map(|c| ((c[0] as u16) << 8) | c[1] as u16).take_while(|&u| u != 0).collect();
            String::from_utf16_lossy(&units)
        }
        4 => data.iter().take_while(|&&b| b != 0).map(|&b| b as char).collect(),
        _ => data.iter().take_while(|&&b| b != 0).map(|&b| ebu_char(b)).filter(|&c| c != '\0').collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_keep_their_place() {
        assert_eq!(decode(b"Wiadomosci\x0AGodzina\x1F12", 0), "Wiadomosci\nGodzina\u{AD}12");
        let chars: Vec<char> = decode(b"Wiadomosci\x0AGodzina 12", 0).chars().collect();
        assert_eq!(chars[11], 'G');
    }

    #[test]
    fn polish_letters() {
        // a few Polish letters of the EBU Latin table
        assert_eq!(ebu_char(0x5E), 'Ł');
        assert_eq!(ebu_char(0x24), 'ł');
        assert_eq!(ebu_char(0x10), 'ą');
        assert_eq!(ebu_char(0xFC), 'ś');
        assert_eq!(decode(b"Radio  ", 0), "Radio  ");
    }
}
