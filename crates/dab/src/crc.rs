//! CRC helpers and energy dispersal.

/// CRC-16 CCITT as used for FIBs, AUs, data groups and packets:
/// polynomial x^16 + x^12 + x^5 + 1, preset 0xFFFF, result inverted.
pub fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    !crc
}

/// Check a block whose last two bytes hold the inverted CRC-16.
pub fn check_crc16(block: &[u8]) -> bool {
    if block.len() < 2 {
        return false;
    }
    let n = block.len() - 2;
    let crc = ((block[n] as u16) << 8) | block[n + 1] as u16;
    crc16_ccitt(&block[..n]) == crc
}

/// Fire code of DAB+ superframes (TS 102 563 clause 6):
/// G(x) = (x^11 + 1)(x^5 + x^3 + x^2 + x + 1), preset 0, no inversion.
pub fn firecode(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x782F } else { crc << 1 };
        }
    }
    crc
}

/// Check the fire code at the start of a DAB+ superframe.
pub fn check_firecode(sf: &[u8]) -> bool {
    if sf.len() < 11 {
        return false;
    }
    // an all zero header is a valid codeword but means silence/no data
    if sf[..11].iter().all(|&b| b == 0) {
        return false;
    }
    let crc = ((sf[0] as u16) << 8) | sf[1] as u16;
    firecode(&sf[2..11]) == crc
}

/// Energy dispersal PRBS (x^9 + x^5 + 1, all ones preset) as a bit sequence.
pub fn prbs(len: usize) -> Vec<u8> {
    let mut reg: u16 = 0x1FF;
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        let bit = ((reg >> 8) ^ (reg >> 4)) & 1;
        reg = ((reg << 1) | bit) & 0x1FF;
        out.push(bit as u8);
    }
    out
}

/// Pack bits (MSB first) into bytes.
pub fn pack_bits(bits: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(bits.len() / 8);
    for chunk in bits.chunks(8) {
        let mut b = 0u8;
        for (i, &bit) in chunk.iter().enumerate() {
            b |= (bit & 1) << (7 - i);
        }
        out.push(b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prbs_start() {
        // EN 300 401 clause 10: first 16 bits of the PRBS are 0000 0111 1011 1110
        let p = prbs(16);
        assert_eq!(p, vec![0, 0, 0, 0, 0, 1, 1, 1, 1, 0, 1, 1, 1, 1, 1, 0]);
    }

    #[test]
    fn crc_roundtrip() {
        let mut d = b"MagicDAB test block".to_vec();
        let c = crc16_ccitt(&d);
        d.push((c >> 8) as u8);
        d.push(c as u8);
        assert!(check_crc16(&d));
    }
}
