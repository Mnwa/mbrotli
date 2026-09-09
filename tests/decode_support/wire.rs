//! Independent, deliberately small RFC fixture assembler (no encoder/core APIs).
#[derive(Default)]
pub struct Wire {
    bytes: Vec<u8>,
    bit: usize,
    large: bool,
}
impl Wire {
    pub fn window(bits: u8, large: bool) -> Self {
        let mut wire = Self {
            large,
            ..Self::default()
        };
        if large {
            wire.bits(8, 0x11);
            wire.bits(6, u64::from(bits));
        } else if bits == 16 {
            wire.bits(1, 0);
        } else if bits == 17 {
            wire.bits(7, 1);
        } else if bits > 17 {
            wire.bits(4, 1 + (u64::from(bits) - 17) * 2);
        } else {
            wire.bits(7, 1 + (u64::from(bits) - 8) * 16);
        }
        wire
    }
    pub fn bits(&mut self, width: u8, value: u64) {
        assert!(width <= 63 && value >> width == 0);
        for i in 0..usize::from(width) {
            if self.bit.is_multiple_of(8) {
                self.bytes.push(0);
            }
            *self.bytes.last_mut().unwrap() |= (((value >> i) & 1) as u8) << (self.bit % 8);
            self.bit += 1;
        }
    }
    fn align(&mut self) {
        while !self.bit.is_multiple_of(8) {
            self.bits(1, 0);
        }
    }
    fn length(&mut self, length: usize, raw: bool) {
        assert!((1..=1 << 24).contains(&length));
        self.bits(1, 0); // ISLAST
        let nibbles = if length <= 1 << 16 {
            4
        } else if length <= 1 << 20 {
            5
        } else {
            6
        };
        self.bits(2, nibbles - 4);
        self.bits(nibbles as u8 * 4, length as u64 - 1);
        self.bits(1, u64::from(raw));
    }
    pub fn raw(&mut self, bytes: &[u8]) {
        self.raw_header(bytes.len());
        self.bytes.extend_from_slice(bytes);
        self.bit += bytes.len() * 8;
    }
    pub fn raw_header(&mut self, length: usize) {
        self.length(length, true);
        self.align();
    }
    pub fn metadata(&mut self, bytes: &[u8]) {
        self.bits(1, 0);
        self.bits(2, 3);
        self.bits(1, 0);
        let width = if bytes.is_empty() {
            0
        } else if bytes.len() <= 256 {
            1
        } else {
            2
        };
        self.bits(2, width);
        if width != 0 {
            self.bits(width as u8 * 8, bytes.len() as u64 - 1);
        }
        self.align();
        self.bytes.extend_from_slice(bytes);
        self.bit += bytes.len() * 8;
    }
    fn single(&mut self, alphabet: u64, symbol: u64) {
        self.bits(2, 1);
        self.bits(2, 0);
        self.bits((64 - (alphabet - 1).leading_zeros()) as u8, symbol);
    }
    /// One insert-zero command. `decoded` can differ from length for transforms.
    pub fn copy(&mut self, length: usize, distance: u64, decoded: usize) {
        self.length(decoded, false);
        self.bits(3, 0); // One block type for each stream.
        self.bits(6, 0); // NPOSTFIX = NDIRECT = 0.
        self.bits(2, 0); // LSB6 context.
        self.bits(2, 0); // One literal tree and one distance tree.
        self.single(256, 0);
        let bases = [
            2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 14, 18, 22, 30, 38, 54, 70, 102, 134, 198, 326, 582,
            1094, 2118,
        ];
        let widths = [
            0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 7, 8, 9, 10, 24,
        ];
        let code = bases.iter().rposition(|&base| base <= length).unwrap();
        let command = match code {
            0..=7 => 128 + code,
            8..=15 => 192 + code - 8,
            _ => 384 + code - 16,
        };
        self.single(704, command as u64);
        let (symbol, extra_width, extra) = (16..if self.large { 140 } else { 64 })
            .find_map(|symbol| {
                let code = symbol - 16;
                let width = 1 + code / 2;
                let base = ((2 + (code & 1)) << width) - 3;
                (distance >= base && distance - base < 1 << width).then_some((
                    symbol,
                    width,
                    distance.wrapping_sub(base),
                ))
            })
            .unwrap();
        self.single(if self.large { 140 } else { 64 }, symbol);
        self.bits(widths[code], (length - bases[code]) as u64);
        self.bits(extra_width as u8, extra);
    }
    pub fn finish(mut self) -> Vec<u8> {
        self.bits(2, 3);
        self.align();
        self.bytes
    }
    /// Drains byte-aligned headers for a streaming generated payload.
    pub fn take(&mut self) -> Vec<u8> {
        assert!(self.bit.is_multiple_of(8));
        self.bit = 0;
        std::mem::take(&mut self.bytes)
    }
}
