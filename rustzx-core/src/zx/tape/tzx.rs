use crate::{
    error::TapeLoadError,
    host::{LoadableAsset, SeekFrom, SeekableAsset},
    zx::tape::TapeImpl,
    Result,
};
use core::str::from_utf8;
use std::println;

const STD_PILOT_LENGTH: usize = 2168;
const STD_PILOT_PULSES_HEADER: usize = 8063;
const STD_PILOT_PULSES_DATA: usize = 3223;
const STD_SYNC1_LENGTH: usize = 667;
const STD_SYNC2_LENGTH: usize = 735;
const STD_BIT_ONE_LENGTH: usize = 1710;
const STD_BIT_ZERO_LENGTH: usize = 855;
// 1000ms in Tstates
const STD_PAUSE_LENGTH: usize = 3_500_000;
const BUFFER_SIZE: usize = 128;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
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
pub struct TapeTimings {
    pilot_length: usize,
    sync1_length: usize,
    sync2_length: usize,
    bit_0_length: usize,
    bit_1_length: usize,
    pilot_pulses_header: usize,
    pilot_pulses_data: usize,
    pause_length: usize,
    // Used by Turbo speed data block
    pilot_tone_length: Option<usize>,
}

impl TapeTimings {
    pub fn default() -> Self {
        Self {
            pilot_length: STD_PILOT_LENGTH,
            pilot_pulses_header: STD_PILOT_PULSES_HEADER,
            pilot_pulses_data: STD_PILOT_PULSES_DATA,
            sync1_length: STD_SYNC1_LENGTH,
            sync2_length: STD_SYNC2_LENGTH,
            bit_0_length: STD_BIT_ZERO_LENGTH,
            bit_1_length: STD_BIT_ONE_LENGTH,
            pause_length: STD_PAUSE_LENGTH,
            pilot_tone_length: None,
        }
    }
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
    tape_timings: TapeTimings,
    used_bits_in_last_byte: usize,
    bits_to_process_in_byte: usize,
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
            tape_timings: TapeTimings::default(),
            used_bits_in_last_byte: 8,
            bits_to_process_in_byte: 0,
        };
        Ok(tzx)
    }

    fn dump_tape_timings_info(&self, block_size: usize) {
        println!("\tPilot length: {}", self.tape_timings.pilot_length);
        println!("\tSync1 length: {}", self.tape_timings.sync1_length);
        println!("\tSync2 length: {}", self.tape_timings.sync2_length);
        println!("\tBit 0 length: {}", self.tape_timings.bit_0_length);
        println!("\tBit 1 length: {}", self.tape_timings.bit_1_length);
        println!(
            "\tPilot tone length: {:?}",
            self.tape_timings.pilot_tone_length
        );
        println!(
            "\tPilot header length: {}",
            self.tape_timings.pilot_pulses_header
        );
        println!(
            "\tPilot data length: {}",
            self.tape_timings.pilot_pulses_data
        );
        println!("\tBits in last byte: {}", self.used_bits_in_last_byte);
        println!(
            "\tPause after block: {}, Block data size: {block_size}",
            self.tape_timings.pause_length
        );
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
        print!("Block {0:0x}: ", block_id);

        match block_id {
            0x10 => {
                println!("Standard speed data block");
                let mut block_header = [0u8; 4];
                if self.asset.read_exact(&mut block_header).is_err() {
                    self.tape_ended = true;
                    return Ok(false);
                }
                self.tape_timings.pause_length =
                    u16::from_le_bytes([block_header[0], block_header[1]]) as usize;
                let block_size = u16::from_le_bytes([block_header[2], block_header[3]]) as usize;
                self.dump_tape_timings_info(block_size);
                let block_bytes_to_read = block_size.min(BUFFER_SIZE);
                self.asset
                    .read_exact(&mut self.buffer[0..block_bytes_to_read])?;
                self.current_block_id = Some(TzxBlockId::StandardSpeedData);
                self.current_block_size = Some(block_size);
            }
            0x11 => {
                println!("Turbo speed data block");
                let mut block_header = [0u8; 18];
                if self.asset.read_exact(&mut block_header).is_err() {
                    self.tape_ended = true;
                    return Ok(false);
                }
                self.tape_timings.pilot_length =
                    u16::from_le_bytes([block_header[0], block_header[1]]) as usize;
                self.tape_timings.sync1_length =
                    u16::from_le_bytes([block_header[2], block_header[3]]) as usize;
                self.tape_timings.sync2_length =
                    u16::from_le_bytes([block_header[4], block_header[5]]) as usize;
                self.tape_timings.bit_0_length =
                    u16::from_le_bytes([block_header[6], block_header[7]]) as usize;
                self.tape_timings.bit_1_length =
                    u16::from_le_bytes([block_header[8], block_header[9]]) as usize;
                self.tape_timings.pilot_tone_length =
                    Some(u16::from_le_bytes([block_header[10], block_header[11]]) as usize);
                self.used_bits_in_last_byte = block_header[12] as usize;
                self.tape_timings.pause_length =
                    u16::from_le_bytes([block_header[13], block_header[14]]) as usize;
                let block_size =
                    u32::from_le_bytes([block_header[15], block_header[16], block_header[17], 0])
                        as usize;
                self.dump_tape_timings_info(block_size);
                let block_bytes_to_read = block_size.min(BUFFER_SIZE);
                self.asset
                    .read_exact(&mut self.buffer[0..block_bytes_to_read])?;
                self.current_block_id = Some(TzxBlockId::TurboSpeedData);
                self.current_block_size = Some(block_size);
            }
            0x19 | 0x16 | 0x17 | 0x34 | 0x35 | 0x40 => {
                println!("Ignoring deprecated block.")
            }
            0x30 => {
                println!("Text Description");
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
                let text_desc_bytes = &self.buffer[0..num_chars as usize];
                let text_desc_str = from_utf8(text_desc_bytes).unwrap();
                println!("\t{text_desc_str}");
                self.current_block_id = Some(TzxBlockId::TextDescription);
                return Ok(true);
            }
            _ => {
                println!("Skipping unknown block!");
                return Ok(true);
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
            //println!("Current state: {:?}", self.state);
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
                    //self.rewind()?;
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
                    if let Some(block_id) = &self.current_block_id {
                        match block_id {
                            TzxBlockId::StandardSpeedData => {
                                let first_byte = self
                                    .next_block_byte()?
                                    .ok_or(TapeLoadError::InvalidTzxFile)?;

                                // Select appropriate pulse count for Pilot sequence
                                let pulses_left = if first_byte == 0x00 {
                                    self.tape_timings.pilot_pulses_header
                                    //STD_PILOT_PULSES_HEADER
                                } else {
                                    self.tape_timings.pilot_pulses_data
                                    //STD_PILOT_PULSES_DATA
                                };
                                self.curr_byte = first_byte;
                                self.curr_bit = true;
                                self.delay = self.tape_timings.pilot_length; // STD_PILOT_LENGTH;
                                self.state = TapeState::Pilot { pulses_left };
                                break 'state_machine;
                            }
                            TzxBlockId::TurboSpeedData => {
                                let first_byte = self
                                    .next_block_byte()?
                                    .ok_or(TapeLoadError::InvalidTzxFile)?;

                                // Select appropriate pulse count for Pilot sequence
                                let pulses_left = self.tape_timings.pilot_tone_length.unwrap();
                                self.curr_byte = first_byte;
                                self.curr_bit = true;
                                self.delay = self.tape_timings.pilot_length;
                                self.state = TapeState::Pilot { pulses_left };
                                break 'state_machine;
                            }
                            _ => {
                                println!("\tProcessing block {:?}", block_id);
                                self.state = TapeState::Play;
                            }
                        }
                    }
                }
                TapeState::Pilot { mut pulses_left } => {
                    self.curr_bit = !self.curr_bit;
                    pulses_left -= 1;
                    if pulses_left == 0 {
                        self.delay = self.tape_timings.sync1_length; // STD_SYNC1_LENGTH;
                        self.state = TapeState::Sync;
                    } else {
                        self.delay = self.tape_timings.pilot_length; // STD_PILOT_LENGTH;
                        self.state = TapeState::Pilot { pulses_left };
                    }
                    break 'state_machine;
                }
                TapeState::Sync => {
                    self.curr_bit = !self.curr_bit;
                    self.delay = self.tape_timings.sync2_length; // STD_SYNC2_LENGTH;
                    self.state = TapeState::NextBit { mask: 0x80 };
                    break 'state_machine;
                }
                TapeState::NextByte => {
                    self.state = if let Some(byte) = self.next_block_byte()? {
                        self.curr_byte = byte;
                        if let Some(block_size) = self.current_block_size {
                            // This is the last byte in block
                            if self.block_bytes_read >= block_size {
                                self.bits_to_process_in_byte = self.used_bits_in_last_byte;
                            }
                        } else {
                            self.bits_to_process_in_byte = 8;
                        }

                        TapeState::NextBit { mask: 0x80 }
                    } else {
                        TapeState::Pause
                    }
                }
                TapeState::NextBit { mask } => {
                    self.curr_bit = !self.curr_bit;
                    if (self.curr_byte & mask) == 0 {
                        self.delay = self.tape_timings.bit_0_length; // STD_BIT_ZERO_LENGTH;
                        self.state = TapeState::BitHalf {
                            half_bit_delay: self.tape_timings.bit_0_length, //STD_BIT_ZERO_LENGTH,
                            mask,
                        };
                    } else {
                        self.delay = self.tape_timings.bit_1_length; //STD_BIT_ONE_LENGTH;
                        self.state = TapeState::BitHalf {
                            half_bit_delay: self.tape_timings.bit_1_length, // STD_BIT_ONE_LENGTH,
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
                    self.bits_to_process_in_byte -= 1;
                    self.state = if mask == 0 || self.bits_to_process_in_byte == 0 {
                        TapeState::NextByte
                    } else {
                        TapeState::NextBit { mask }
                    };
                    break 'state_machine;
                }
                TapeState::Pause => {
                    self.curr_bit = !self.curr_bit;
                    self.delay = self.tape_timings.pause_length * 3_500; // STD_PAUSE_LENGTH;
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
