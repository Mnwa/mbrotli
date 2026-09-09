use super::super::{DecodeError, InvalidDataKind};
use super::{
    bits::{Bits, Input},
    huffman::{Builder, Huffman},
    memory::Memory,
};
use alloc::vec::Vec;

/// Each phase commits only complete fields; pending repeats retain their width.
#[derive(Debug, Default)]
enum Stage {
    #[default]
    TreeCount,
    RunPrefix,
    Tree,
    Entries,
    Repeat {
        width: u8,
    },
    Transform,
    Complete,
}

#[derive(Debug, Default)]
pub(super) struct ContextMap {
    pub(super) values: Vec<u8>,
    pub(super) trees: usize,
    stage: Stage,
    run: u8,
    index: usize,
    tree: Huffman,
}

impl ContextMap {
    pub(super) fn reset(&mut self) {
        self.stage = Stage::TreeCount;
        self.values.clear();
    }

    pub(super) fn read(
        &mut self,
        size: usize,
        bits: &mut Bits,
        input: &mut Input<'_>,
        builder: &mut Builder,
        memory: &mut Memory,
    ) -> Result<bool, DecodeError> {
        loop {
            match self.stage {
                Stage::TreeCount => {
                    let Some(n) = bits.uint8(input)? else {
                        return Ok(false);
                    };
                    self.trees = n + 1;
                    self.index = 0;
                    memory.resize(&mut self.values, size)?;
                    self.values.fill(0);
                    if n == 0 {
                        self.stage = Stage::Complete;
                        return Ok(true);
                    }
                    self.stage = Stage::RunPrefix;
                }
                Stage::RunPrefix => {
                    let Some(flag) = bits.peek(1, input)? else {
                        return Ok(false);
                    };
                    self.run = if flag == 0 {
                        bits.drop(1);
                        0
                    } else {
                        let Some(n) = bits.read(5, input)? else {
                            return Ok(false);
                        };
                        (n >> 1) as u8 + 1
                    };
                    self.stage = Stage::Tree;
                }
                Stage::Tree => {
                    if !builder.read(
                        self.trees + usize::from(self.run),
                        bits,
                        input,
                        &mut self.tree,
                    )? {
                        return Ok(false);
                    }
                    self.stage = Stage::Entries;
                }
                Stage::Entries => {
                    if self.index == size {
                        self.stage = Stage::Transform;
                        continue;
                    }
                    let Some(symbol) = self.tree.read(bits, input)? else {
                        return Ok(false);
                    };
                    if symbol != 0 && symbol <= usize::from(self.run) {
                        self.stage = Stage::Repeat {
                            width: symbol as u8,
                        };
                        continue;
                    }
                    let value = if symbol == 0 {
                        0
                    } else {
                        symbol - usize::from(self.run)
                    };
                    if value >= self.trees {
                        return Err(InvalidDataKind::ContextMap.into());
                    }
                    self.values[self.index] = value as u8;
                    self.index += 1;
                }
                Stage::Repeat { width } => {
                    let Some(extra) = bits.read(width, input)? else {
                        return Ok(false);
                    };
                    let end = self.index + (1 << width) + extra as usize;
                    if end > size {
                        return Err(InvalidDataKind::ContextMap.into());
                    }
                    self.values[self.index..end].fill(0);
                    self.index = end;
                    self.stage = Stage::Entries;
                }
                Stage::Transform => {
                    let Some(mtf) = bits.read(1, input)? else {
                        return Ok(false);
                    };
                    if mtf != 0 {
                        let mut symbols = [0u8; 256];
                        for (i, value) in symbols.iter_mut().enumerate() {
                            *value = i as u8;
                        }
                        for value in &mut self.values {
                            let index = usize::from(*value);
                            *value = symbols[index];
                            symbols[..=index].rotate_right(1);
                        }
                    }
                    self.stage = Stage::Complete;
                    return Ok(true);
                }
                Stage::Complete => return Ok(true),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures;
    use super::*;

    #[test]
    fn encoded_zero_runs_and_move_to_front_have_observable_results() {
        let cases = [
            (
                fixtures::fields(&[
                    (4, 1),
                    (5, 1),
                    (2, 1),
                    (2, 0),
                    (2, 1),
                    (1, 0),
                    (1, 0),
                    (1, 1),
                ]),
                [0, 0, 0, 0],
            ),
            (
                fixtures::fields(&[
                    (4, 1),
                    (1, 0),
                    (2, 1),
                    (2, 1),
                    (1, 0),
                    (1, 1),
                    (4, 0b0101),
                    (1, 1),
                ]),
                [1, 1, 0, 0],
            ),
        ];
        for (bytes, expected) in cases {
            let mut map = ContextMap::default();
            assert!(
                map.read(
                    4,
                    &mut Bits::default(),
                    &mut fixtures::input(&bytes),
                    &mut Builder::default(),
                    &mut Memory::default()
                )
                .unwrap()
            );
            assert_eq!(map.trees, 2);
            assert_eq!(map.values, expected);
        }
    }
}
