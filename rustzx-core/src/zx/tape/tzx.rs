use crate::{
    error::TapeLoadError,
    host::{LoadableAsset, SeekFrom, SeekableAsset},
    zx::tape::TapeImpl,
    Result,
};
use core::str::from_utf8;
use std::println;

const PILOT_LENGTH: usize = 2168;
const PILOT_PULSES_HEADER: usize = 8063;
const PILOT_PULSES_DATA: usize = 3223;
const SYNC1_LENGTH: usize = 667;
const SYNC2_LENGTH: usize = 735;
const BIT_ONE_LENGTH: usize = 1710;
const BIT_ZERO_LENGTH: usize = 855;
const PAUSE_LENGTH: usize = 3_500_000;
const BUFFER_SIZE: usize = 128;

#[derive(PartialEq, Eq, Clone, Copy)]
enum TapeState {
    Init,
    Stop,
    Play,
    Process,
    Pilot { pulses_left: usize },
    Sync,
    NextByte,
    NextBit { mask: u8 },
    BitHalf { half_bit_delay: usize, mask: u8 },
    Pause,
}

// Tzx block id's are in hex
#[derive(Debug)]
pub enum TzxBlockId {
    StandardSpeedData = 0x10,
    TurboSpeedData = 0x11,
    PureTone = 0x12,
    PulseSequence = 0x13,
    PureDataBlock = 0x14,
    DirectRecording = 0x15,

    C64RomTypeData = 0x16,   // Deprecated
    C64TurboTapeData = 0x17, // Deprecated

    CswRecording = 0x18,
    GeneralizedData = 0x19,
    PauseOrSilence = 0x20,
    GroupStart = 0x21,
    GroupEnd = 0x22,
    JumpToBlock = 0x23,
    LoopStart = 0x24,
    LoopEnd = 0x25,
    CallSequence = 0x26,
    ReturnFromSequence = 0x27,
    SelectBlock = 0x28,
    StopIf48k = 0x2a,
    SetSignalLevel = 0x2b,
    TextDescription = 0x30,
    MessageBlock = 0x31,
    ArchiveInfo = 0x32,
    HardwareType = 0x33,
    EmulationInfo = 0x34, // Deprecated
    CustomInfo = 0x35,    // Deprecated
    Snapshot = 0x40,      // Deprecated
    Glue = 0x5a,
}

pub struct Tzx<A: LoadableAsset + SeekableAsset> {
    asset: A,
    state: TapeState,
    prev_state: TapeState,
    buffer: [u8; BUFFER_SIZE],
    buffer_offset: usize,
    block_bytes_read: usize,
    previous_block_id: Option<TzxBlockId>,
    current_block_id: Option<TzxBlockId>,
    current_block_size: Option<usize>,
    tape_ended: bool,
    // Non-fastload related fields
    curr_bit: bool,
    curr_byte: u8,
    delay: usize,
    pause_after_block: u16,
    is_processing_data_block: bool,
}

impl<A: LoadableAsset + SeekableAsset> Tzx<A> {
    pub fn from_asset(asset: A) -> Result<Self> {
        let tzx = Self {
            prev_state: TapeState::Stop,
            state: TapeState::Init,
            curr_bit: false,
            curr_byte: 0x00,
            buffer: [0u8; BUFFER_SIZE],
            buffer_offset: 0,
            block_bytes_read: 0,
            previous_block_id: None,
            current_block_id: None,
            current_block_size: None,
            delay: 0,
            asset,
            tape_ended: false,
            pause_after_block: 0,
            is_processing_data_block: false,
        };
        Ok(tzx)
    }

    fn next_tzx_block(&mut self) -> Result<bool> {
        println!("Next TZX block");
        if self.tape_ended {
            return Ok(false);
        }

        let mut id_size_buffer = [0u8; 1];
        if self.asset.read_exact(&mut id_size_buffer).is_err() {
            self.tape_ended = true;
            return Ok(false);
        }

        let block_id = id_size_buffer[0];
        self.buffer_offset = 0;
        self.block_bytes_read = 0;
        println!("Block: {block_id}");

        match block_id {
            0x10 => {
                println!("\tStandard speed data block");
                let mut block_header = [0u8; 4];
                if self.asset.read_exact(&mut block_header).is_err() {
                    self.tape_ended = true;
                    return Ok(false);
                }
                self.pause_after_block = u16::from_le_bytes([block_header[0], block_header[1]]);
                let block_size = u16::from_le_bytes([block_header[2], block_header[3]]) as usize;
                println!(
                    "\tPause after block: {}, Block data size: {block_size}",
                    self.pause_after_block
                );
                let block_bytes_to_read = block_size.min(BUFFER_SIZE);
                self.asset
                    .read_exact(&mut self.buffer[0..block_bytes_to_read])?;
                self.current_block_id = Some(TzxBlockId::StandardSpeedData);
            }
            0x19 | 0x16 | 0x17 | 0x34 | 0x35 | 0x40 => {
                println!("\tIgnoring deprecated block.")
            }
            0x39 => {
                println!("\tText Description");
                let mut num_chars_header = [0u8; 1];
                if self.asset.read_exact(&mut num_chars_header).is_err() {
                    self.tape_ended = true;
                    return Ok(false);
                }
                let num_chars = num_chars_header[0];
                if self
                    .asset
                    .read_exact(&mut self.buffer[0..num_chars as usize])
                    .is_err()
                {
                    self.tape_ended = true;
                    return Ok(false);
                }
            }
        }

        Ok(true)
    }
}

impl<A: LoadableAsset + SeekableAsset> TapeImpl for Tzx<A> {
    fn can_fast_load(&self) -> bool {
        false
        //self.state == TapeState::Stop
    }

    fn next_block_byte(&mut self) -> Result<Option<u8>> {
        if self.tape_ended {
            return Ok(None);
        }

        if let Some(block_size) = self.current_block_size {
            if self.block_bytes_read >= block_size {
                return Ok(None);
            }

            let mut buffer_read_pos = self.block_bytes_read - self.buffer_offset;

            // Read new buffer if required
            if buffer_read_pos >= BUFFER_SIZE {
                let bytes_to_read =
                    (block_size - self.buffer_offset - BUFFER_SIZE).min(BUFFER_SIZE);
                self.asset.read_exact(&mut self.buffer[0..bytes_to_read])?;
                self.buffer_offset += BUFFER_SIZE;
                buffer_read_pos = 0;
            }

            // Check last byte in block
            if self.block_bytes_read >= block_size {
                self.current_block_size = None;
                self.block_bytes_read = 0;
                return Ok(None);
            }

            // Perform actual read and advance position
            let result = self.buffer[buffer_read_pos];
            self.block_bytes_read += 1;
            return Ok(Some(result));
        }

        Ok(None)
    }

    fn next_block(&mut self) -> Result<bool> {
        if self.tape_ended {
            return Ok(false);
        }

        if self.is_processing_data_block {
            // Skip leftovers from the previous block
            while self.next_block_byte()?.is_some() {}

            let mut block_size_buffer = [0u8; 2];
            if self.asset.read_exact(&mut block_size_buffer).is_err() {
                self.tape_ended = true;
                return Ok(false);
            }
            let block_size = u16::from_le_bytes(block_size_buffer) as usize;
            let block_bytes_to_read = block_size.min(BUFFER_SIZE);
            self.asset
                .read_exact(&mut self.buffer[0..block_bytes_to_read])?;

            self.buffer_offset = 0;
            self.block_bytes_read = 0;
            self.current_block_size = Some(block_size);

            Ok(true)
        }
        Ok(true)
    }

    fn current_bit(&self) -> bool {
        self.curr_bit
    }

    fn process_clocks(&mut self, clocks: usize) -> Result<()> {
        if self.state == TapeState::Stop {
            return Ok(());
        }

        if self.delay > 0 {
            if clocks > self.delay {
                self.delay = 0;
            } else {
                self.delay -= clocks;
            }
            return Ok(());
        }

        'state_machine: loop {
            match self.state {
                TapeState::Init => {
                    const HEADER_SIZE: usize = 10;
                    // check if valid tzx
                    let mut header_size_buffer = [0u8; HEADER_SIZE];
                    self.asset.seek(SeekFrom::Start(0))?;
                    if self
                        .asset
                        .read_exact(&mut header_size_buffer[0..HEADER_SIZE])
                        .is_ok()
                    {
                        let signature = &header_size_buffer[0..8];
                        let signature_str = from_utf8(signature).unwrap();
                        println!("Signature: {signature_str}");
                        let major_version = header_size_buffer[8];
                        let minor_version = header_size_buffer[9];
                        println!("TZX Version: {major_version}.{minor_version}");
                        self.state = TapeState::Play;
                    } else {
                        println!("Error: Failed to read TZX file header.");
                        return Err(TapeLoadError::InvalidTapFile.into());
                    }
                    self.buffer_offset += HEADER_SIZE;
                    break 'state_machine;
                }
                TapeState::Stop => {
                    // Reset tape but leave in Stopped state
                    self.rewind()?;
                    self.state = TapeState::Stop;
                    break 'state_machine;
                }
                TapeState::Play => {
                    if !self.next_tzx_block()? {
                        self.state = TapeState::Stop;
                    } else {
                        self.state = TapeState::Process;
                    }
                }
                TapeState::Process => {
                    if let Some(block_id) = self.current_block_id {
                        match block_id {
                            _ => {
                                println!("\tUnknown block {:?}", block_id);
                                self.state = TapeState::Play;
                            }
                        }
                    }
                }
                TapeState::Pilot { mut pulses_left } => {
                    self.curr_bit = !self.curr_bit;
                    pulses_left -= 1;
                    if pulses_left == 0 {
                        self.delay = SYNC1_LENGTH;
                        self.state = TapeState::Sync;
                    } else {
                        self.delay = PILOT_LENGTH;
                        self.state = TapeState::Pilot { pulses_left };
                    }
                    break 'state_machine;
                }
                TapeState::Sync => {
                    self.curr_bit = !self.curr_bit;
                    self.delay = SYNC2_LENGTH;
                    self.state = TapeState::NextBit { mask: 0x80 };
                    break 'state_machine;
                }
                TapeState::NextByte => {
                    self.state = if let Some(byte) = self.next_block_byte()? {
                        self.curr_byte = byte;
                        TapeState::NextBit { mask: 0x80 }
                    } else {
                        TapeState::Pause
                    }
                }
                TapeState::NextBit { mask } => {
                    self.curr_bit = !self.curr_bit;
                    if (self.curr_byte & mask) == 0 {
                        self.delay = BIT_ZERO_LENGTH;
                        self.state = TapeState::BitHalf {
                            half_bit_delay: BIT_ZERO_LENGTH,
                            mask,
                        };
                    } else {
                        self.delay = BIT_ONE_LENGTH;
                        self.state = TapeState::BitHalf {
                            half_bit_delay: BIT_ONE_LENGTH,
                            mask,
                        };
                    };
                    break 'state_machine;
                }
                TapeState::BitHalf {
                    half_bit_delay,
                    mut mask,
                } => {
                    self.curr_bit = !self.curr_bit;
                    self.delay = half_bit_delay;
                    mask >>= 1;
                    self.state = if mask == 0 {
                        TapeState::NextByte
                    } else {
                        TapeState::NextBit { mask }
                    };
                    break 'state_machine;
                }
                TapeState::Pause => {
                    self.curr_bit = !self.curr_bit;
                    self.delay = PAUSE_LENGTH;
                    // Next block or end of the tape
                    self.state = TapeState::Play;
                    break 'state_machine;
                }
            }
        }

        Ok(())
    }

    fn stop(&mut self) {
        let state = self.state;
        self.prev_state = state;
        self.state = TapeState::Stop;
    }

    fn play(&mut self) {
        println!("Attempting to play");
        if self.state == TapeState::Stop {
            if self.prev_state == TapeState::Stop {
                self.state = TapeState::Play;
            } else {
                self.state = self.prev_state;
            }
        }
    }

    fn rewind(&mut self) -> Result<()> {
        println!("Rewinding tape");
        self.curr_bit = false;
        self.curr_byte = 0x00;
        self.block_bytes_read = 0;
        self.buffer_offset = 0;
        self.current_block_size = None;
        self.delay = 0;
        self.asset.seek(SeekFrom::Start(0))?;
        self.tape_ended = false;
        Ok(())
    }
}
