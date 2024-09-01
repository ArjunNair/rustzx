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
    PureTone { pulses_left: usize },
    PulseSequence { pulses_left: usize },
    Sync,
    NextByte { is_direct_recording_sample: bool },
    NextBit { mask: u8 },
    NextDirectRecordingBit { mask: u8 },
    BitHalf { half_bit_delay: usize, mask: u8 },
    Pause,
    Silence { length: usize },
}

// Tzx block id's are in hex
#[derive(Debug, Clone, Copy)]
pub enum TzxBlockId {
    Unknown = 0x0,
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
    current_block_id: Option<TzxBlockId>,
    current_block_size: Option<usize>,
    tape_ended: bool,
    // Non-fastload related fields
    curr_bit: bool,
    curr_byte: u8,
    delay: isize,
    tape_timings: TapeTimings,
    used_bits_in_last_byte: usize,
    bits_to_process_in_byte: usize,
    loop_start_marker: usize,
    num_repetitions: Option<u16>,
    is_48k_mode: bool,
}

impl<A: LoadableAsset + SeekableAsset> Tzx<A> {
    pub fn from_asset(asset: A, is48k: bool) -> Result<Self> {
        let tzx = Self {
            prev_state: TapeState::Stop,
            state: TapeState::Init,
            curr_bit: false,
            curr_byte: 0x00,
            buffer: [0u8; BUFFER_SIZE],
            buffer_offset: 0,
            block_bytes_read: 0,
            current_block_id: None,
            current_block_size: None,
            delay: 0,
            asset,
            tape_ended: false,
            tape_timings: TapeTimings::default(),
            used_bits_in_last_byte: 8,
            bits_to_process_in_byte: 0,
            loop_start_marker: 0,
            num_repetitions: None,
            is_48k_mode: is48k,
        };
        Ok(tzx)
    }

    fn dump_pilot_pulse_info(&self) {
        println!("\tPilot length: {}", self.tape_timings.pilot_length);
        println!(
            "\tPilot tone length: {:?}",
            self.tape_timings.pilot_tone_length
        );
    }

    fn dump_bit_pulse_info(&self) {
        println!("\tBit 0 length: {}", self.tape_timings.bit_0_length);
        println!("\tBit 1 length: {}", self.tape_timings.bit_1_length);
    }

    fn dump_tape_timings_info(&self, block_size: usize) {
        self.dump_pilot_pulse_info();
        println!("\tSync1 length: {}", self.tape_timings.sync1_length);
        println!("\tSync2 length: {}", self.tape_timings.sync2_length);
        self.dump_bit_pulse_info();
        println!(
            "\tPilot header length: {}",
            self.tape_timings.pilot_pulses_header
        );
        println!(
            "\tPilot data length: {}",
            self.tape_timings.pilot_pulses_data
        );
        println!("\tBits in last byte: {}", self.used_bits_in_last_byte);
        if block_size > 0 {
            println!(
                "\tPause after block: {}, Block data size: {block_size}",
                self.tape_timings.pause_length
            );
        }
    }
}

impl<A: LoadableAsset + SeekableAsset> TapeImpl for Tzx<A> {
    fn can_fast_load(&self) -> bool {
        self.state == TapeState::Stop
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

            if self.block_bytes_read >= block_size {
                self.bits_to_process_in_byte = self.used_bits_in_last_byte;
                //println!("\tBits to process: {}", self.bits_to_process_in_byte);
            } else {
                self.bits_to_process_in_byte = 8;
            }
            return Ok(Some(result));
        }

        Ok(None)
    }

    fn next_block(&mut self) -> Result<bool> {
        //println!("Next TZX block");
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
                self.tape_timings.pilot_length = STD_PILOT_LENGTH;
                self.tape_timings.sync1_length = STD_SYNC1_LENGTH;
                self.tape_timings.sync2_length = STD_SYNC2_LENGTH;
                self.tape_timings.pilot_pulses_header = STD_PILOT_PULSES_HEADER;
                self.tape_timings.pilot_pulses_data = STD_PILOT_PULSES_DATA;
                self.tape_timings.bit_0_length = STD_BIT_ZERO_LENGTH;
                self.tape_timings.bit_1_length = STD_BIT_ONE_LENGTH;
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
            0x12 => {
                println!("Pure tone");
                let mut block_header = [0u8; 4];
                if self.asset.read_exact(&mut block_header).is_err() {
                    self.tape_ended = true;
                    return Ok(false);
                }
                self.tape_timings.pilot_length =
                    u16::from_le_bytes([block_header[0], block_header[1]]) as usize;
                self.tape_timings.pilot_tone_length =
                    Some(u16::from_le_bytes([block_header[2], block_header[3]]) as usize);

                self.dump_pilot_pulse_info();
                self.current_block_id = Some(TzxBlockId::PureTone);
                return Ok(true);
            }
            0x13 => {
                println!("Pulse sequence");
                let mut block_header = [0u8; 1];
                if self.asset.read_exact(&mut block_header).is_err() {
                    self.tape_ended = true;
                    return Ok(false);
                }
                self.tape_timings.pilot_tone_length = Some(block_header[0] as usize);
                println!(
                    "\tPilot tone length: {:?}",
                    self.tape_timings.pilot_tone_length
                );
                let block_size = (block_header[0] as usize) * 2;
                self.dump_tape_timings_info(block_size);
                let block_bytes_to_read = block_size.min(BUFFER_SIZE);
                self.asset
                    .read_exact(&mut self.buffer[0..block_bytes_to_read])?;
                self.current_block_size = Some(block_size);
                self.current_block_id = Some(TzxBlockId::PulseSequence);
                return Ok(true);
            }
            0x14 => {
                println!("Pure data block");
                let mut block_header = [0u8; 10];
                if self.asset.read_exact(&mut block_header).is_err() {
                    self.tape_ended = true;
                    return Ok(false);
                }
                self.tape_timings.bit_0_length =
                    u16::from_le_bytes([block_header[0], block_header[1]]) as usize;
                self.tape_timings.bit_1_length =
                    u16::from_le_bytes([block_header[2], block_header[3]]) as usize;
                self.tape_timings.pilot_tone_length = None;
                self.used_bits_in_last_byte = block_header[4] as usize;
                self.tape_timings.pause_length =
                    u16::from_le_bytes([block_header[5], block_header[6]]) as usize;
                let block_size =
                    u32::from_le_bytes([block_header[7], block_header[8], block_header[9], 0])
                        as usize;
                self.dump_bit_pulse_info();
                println!("\tPause length: {}", self.tape_timings.pause_length);
                let block_bytes_to_read = block_size.min(BUFFER_SIZE);
                self.asset
                    .read_exact(&mut self.buffer[0..block_bytes_to_read])?;
                self.current_block_id = Some(TzxBlockId::PureDataBlock);
                self.current_block_size = Some(block_size);
            }
            0x15 => {
                println!("Direct Recording Block");
                let mut block_header = [0u8; 8];
                if self.asset.read_exact(&mut block_header).is_err() {
                    self.tape_ended = true;
                    return Ok(false);
                }
                self.tape_timings.bit_0_length =
                    u16::from_le_bytes([block_header[0], block_header[1]]) as usize;
                self.tape_timings.pause_length =
                    u16::from_le_bytes([block_header[2], block_header[3]]) as usize;
                self.used_bits_in_last_byte = block_header[4] as usize;
                let block_size =
                    u32::from_le_bytes([block_header[5], block_header[6], block_header[7], 0])
                        as usize;
                let block_bytes_to_read = block_size.min(BUFFER_SIZE);
                println!("\tNum t-states per sample: {}", {
                    self.tape_timings.bit_0_length
                });
                println!("\tPause after block: {}", {
                    self.tape_timings.pause_length
                });
                println!("\tBits in last byte: {}", self.used_bits_in_last_byte);
                println!("\tBlock size: {}", block_size);
                if self
                    .asset
                    .read_exact(&mut self.buffer[0..block_bytes_to_read as usize])
                    .is_err()
                {
                    self.tape_ended = true;
                    return Ok(false);
                }
                self.current_block_id = Some(TzxBlockId::DirectRecording);
                self.current_block_size = Some(block_size);
                return Ok(true);
            }
            0x16 | 0x17 | 0x18 | 0x19 | 0x2b => {
                println!("Unsupported block");
                let mut block_header = [0u8; 4];
                if self.asset.read_exact(&mut block_header).is_err() {
                    self.tape_ended = true;
                    return Ok(false);
                }
                let block_size = u32::from_le_bytes(block_header) as isize;
                self.asset.seek(SeekFrom::Current(block_size))?;
                self.current_block_id = Some(TzxBlockId::Unknown);
                return Ok(true);
            }
            0x34 | 0x35 | 0x40 => {
                println!("Ignoring deprecated block.");
            }
            0x20 => {
                println!("Pause or Stop command");
                let mut block_header = [0u8; 2];
                if self.asset.read_exact(&mut block_header).is_err() {
                    self.tape_ended = true;
                    return Ok(false);
                }
                let val = u16::from_le_bytes(block_header) as usize;
                //println!("\tPause length: {val}");
                // Stop tape
                if val == 0 {
                    self.delay = 0;
                    return Ok(false);
                }
                // Save the value to buffer for later read by pause block
                self.buffer[0] = block_header[0];
                self.buffer[1] = block_header[1];
                self.current_block_id = Some(TzxBlockId::PauseOrSilence);
                return Ok(true);
            }
            0x21 => {
                println!("Group start");
                let mut block_header = [0u8; 1];
                if self.asset.read_exact(&mut block_header).is_err() {
                    self.tape_ended = true;
                    return Ok(false);
                }
                let num_chars = block_header[0];
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
                self.current_block_id = Some(TzxBlockId::GroupStart);
                return Ok(true);
            }
            0x22 => {
                println!("Group end");
                self.current_block_id = Some(TzxBlockId::GroupEnd);
                return Ok(true);
            }
            0x24 => {
                println!("Loop start");
                let mut block_header = [0u8; 2];
                if self.asset.read_exact(&mut block_header).is_err() {
                    self.tape_ended = true;
                    return Ok(false);
                }
                self.num_repetitions = Some(u16::from_le_bytes(block_header));
                println!("\tNum iterations: {:?}", self.num_repetitions);
                self.loop_start_marker = self.asset.seek(SeekFrom::Current(0))?;
            }
            0x25 => {
                println!("Loop end");
                self.current_block_id = Some(TzxBlockId::LoopEnd);
                if let Some(mut num_rep) = self.num_repetitions {
                    println!("\tRepetitions left: {num_rep}");
                    if num_rep > 0 {
                        num_rep -= 1;
                        self.num_repetitions = Some(num_rep);
                        self.asset.seek(SeekFrom::Start(self.loop_start_marker))?;
                        return Ok(true);
                    }
                }
                self.num_repetitions = None;
                return Ok(true);
            }
            0x28 => {
                println!("Select block");
                let mut block_header = [0u8; 2];
                if self.asset.read_exact(&mut block_header).is_err() {
                    self.tape_ended = true;
                    return Ok(false);
                }
                let block_size = u16::from_le_bytes(block_header) as isize;
                self.asset.seek(SeekFrom::Current(block_size))?;
                self.current_block_id = Some(TzxBlockId::Unknown);
                return Ok(true);
            }
            0x2A => {
                println!("Stop tape if 48k mode");
                let mut block_header = [0u8; 4];
                if self.asset.read_exact(&mut block_header).is_err() {
                    self.tape_ended = true;
                    return Ok(false);
                }
                if self.is_48k_mode {
                    println!("\t48k mode detected!");
                    return Ok(false);
                }
                self.current_block_id = Some(TzxBlockId::StopIf48k);
                return Ok(true);
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
                self.current_block_size = Some(num_chars as usize);
                let text_desc_str = from_utf8(text_desc_bytes).unwrap();
                println!("\t{text_desc_str}");
                self.current_block_id = Some(TzxBlockId::TextDescription);
                return Ok(true);
            }
            0x32 => {
                println!("Archive Info");
                let mut block_header = [0u8; 2];
                if self.asset.read_exact(&mut block_header).is_err() {
                    self.tape_ended = true;
                    return Ok(false);
                }
                let block_size = u16::from_le_bytes(block_header) as isize;
                self.asset.seek(SeekFrom::Current(block_size))?;
                self.current_block_id = Some(TzxBlockId::Unknown);
                return Ok(true);
            }
            _ => {
                println!("Skipping unknown block!");
                return Ok(true);
            }
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
            self.delay -= clocks as isize;
            if self.delay > 0 {
                return Ok(());
            }
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
                    println!("Stopped Tape.");
                    self.state = TapeState::Stop;
                    break 'state_machine;
                }
                TapeState::Play => {
                    if !self.next_block()? {
                        self.state = TapeState::Stop;
                    } else {
                        self.state = TapeState::Process;
                    }
                }
                TapeState::Process => {
                    if self.process_current_block().is_ok() {
                        break 'state_machine;
                    }
                }
                TapeState::Pilot { mut pulses_left } => {
                    self.curr_bit = !self.curr_bit;
                    pulses_left -= 1;
                    if pulses_left == 0 {
                        self.delay += self.tape_timings.sync1_length as isize;
                        self.state = TapeState::Sync;
                    } else {
                        self.delay += self.tape_timings.pilot_length as isize;
                        self.state = TapeState::Pilot { pulses_left };
                    }
                    break 'state_machine;
                }
                TapeState::PureTone { mut pulses_left } => {
                    self.curr_bit = !self.curr_bit;
                    pulses_left -= 1;
                    if pulses_left == 0 {
                        self.state = TapeState::Play;
                    } else {
                        self.delay += self.tape_timings.pilot_length as isize;
                        self.state = TapeState::PureTone { pulses_left };
                    }
                    break 'state_machine;
                }
                TapeState::PulseSequence { mut pulses_left } => {
                    self.curr_bit = !self.curr_bit;
                    pulses_left -= 1;
                    if pulses_left == 0 {
                        self.state = TapeState::Play;
                    } else {
                        // Read 2 bytes for the pulse length
                        let byte1 = self
                            .next_block_byte()?
                            .ok_or(TapeLoadError::InvalidTzxFile)?;
                        let byte2 = self
                            .next_block_byte()?
                            .ok_or(TapeLoadError::InvalidTzxFile)?;

                        self.delay += u16::from_le_bytes([byte1, byte2]) as isize;
                        println!("\tPulse length: {}", self.delay);
                        self.state = TapeState::PulseSequence { pulses_left };
                    }
                    break 'state_machine;
                }
                TapeState::Sync => {
                    self.curr_bit = !self.curr_bit;
                    self.delay += self.tape_timings.sync2_length as isize;
                    self.state = TapeState::NextBit { mask: 0x80 };
                    break 'state_machine;
                }
                TapeState::NextByte {
                    is_direct_recording_sample,
                } => {
                    self.state = if let Some(byte) = self.next_block_byte()? {
                        self.curr_byte = byte;
                        if is_direct_recording_sample {
                            TapeState::NextDirectRecordingBit { mask: 0x80 }
                        } else {
                            TapeState::NextBit { mask: 0x80 }
                        }
                    } else {
                        TapeState::Pause
                    }
                }
                TapeState::NextBit { mask } => {
                    self.curr_bit = !self.curr_bit;

                    if (self.curr_byte & mask) == 0 {
                        self.delay += self.tape_timings.bit_0_length as isize;
                        self.state = TapeState::BitHalf {
                            half_bit_delay: self.tape_timings.bit_0_length,
                            mask,
                        };
                    } else {
                        self.delay += self.tape_timings.bit_1_length as isize;

                        self.state = TapeState::BitHalf {
                            half_bit_delay: self.tape_timings.bit_1_length,
                            mask,
                        };
                    };

                    break 'state_machine;
                }
                // Direct Recording sample processing
                TapeState::NextDirectRecordingBit { mut mask } => {
                    let bit = self.curr_byte & mask == 0;
                    self.delay += self.tape_timings.bit_0_length as isize;

                    if bit != self.curr_bit {
                        self.curr_bit = !self.curr_bit;
                    }
                    mask >>= 1;
                    self.bits_to_process_in_byte -= 1;
                    self.state = if mask == 0 || self.bits_to_process_in_byte == 0 {
                        TapeState::NextByte {
                            is_direct_recording_sample: true,
                        }
                    } else {
                        TapeState::NextDirectRecordingBit { mask }
                    };
                    break 'state_machine;
                }
                TapeState::BitHalf {
                    half_bit_delay,
                    mut mask,
                } => {
                    self.curr_bit = !self.curr_bit;
                    self.delay += half_bit_delay as isize;
                    mask >>= 1;
                    self.bits_to_process_in_byte -= 1;

                    self.state = if mask == 0 || self.bits_to_process_in_byte == 0 {
                        TapeState::NextByte {
                            is_direct_recording_sample: false,
                        }
                    } else {
                        TapeState::NextBit { mask }
                    };
                    break 'state_machine;
                }

                TapeState::Pause => {
                    self.delay += (self.tape_timings.pause_length * 3_500) as isize;
                    self.state = TapeState::Play;
                    self.curr_bit = !self.curr_bit;
                    if self.delay > 0 {
                        break 'state_machine;
                    } // Next block or end of the tape
                }
                TapeState::Silence { length } => {
                    self.curr_bit = !self.curr_bit;
                    self.delay += (length * 3_500) as isize;
                    self.state = TapeState::Play;
                    break 'state_machine;
                }
            }
        }

        Ok(())
    }

    fn process_current_block(&mut self) -> Result<()> {
        if let Some(block_id) = &self.current_block_id.clone() {
            match block_id {
                TzxBlockId::StandardSpeedData => {
                    let first_byte = self
                        .next_block_byte()?
                        .ok_or(TapeLoadError::InvalidTzxFile)?;
                    // Select appropriate pulse count for Pilot sequence
                    let pulses_left = if first_byte == 0x00 {
                        self.tape_timings.pilot_pulses_header
                    } else {
                        self.tape_timings.pilot_pulses_data
                    };
                    self.curr_byte = first_byte;
                    self.delay += self.tape_timings.pilot_length as isize;
                    self.state = TapeState::Pilot { pulses_left };
                }
                TzxBlockId::TurboSpeedData => {
                    let first_byte = self
                        .next_block_byte()?
                        .ok_or(TapeLoadError::InvalidTzxFile)?;

                    // Select appropriate pulse count for Pilot sequence
                    let pulses_left = self.tape_timings.pilot_tone_length.unwrap();
                    self.curr_byte = first_byte;
                    self.delay += self.tape_timings.pilot_length as isize;
                    self.state = TapeState::Pilot { pulses_left };
                }
                TzxBlockId::DirectRecording => {
                    let first_byte = self
                        .next_block_byte()?
                        .ok_or(TapeLoadError::InvalidTzxFile)?;
                    self.curr_byte = first_byte;
                    self.curr_bit = !self.curr_bit;
                    self.state = TapeState::NextDirectRecordingBit { mask: 0x80 };
                }
                TzxBlockId::PureTone => {
                    let pulses_left = self.tape_timings.pilot_tone_length.unwrap();
                    //self.curr_bit = !self.curr_bit;
                    self.delay += self.tape_timings.pilot_length as isize;
                    self.state = TapeState::PureTone { pulses_left };
                }
                TzxBlockId::PulseSequence => {
                    let pulses_left = self.tape_timings.pilot_tone_length.unwrap();

                    // Read 2 bytes for the pulse length
                    let byte1 = self
                        .next_block_byte()?
                        .ok_or(TapeLoadError::InvalidTzxFile)?;
                    let byte2 = self
                        .next_block_byte()?
                        .ok_or(TapeLoadError::InvalidTzxFile)?;

                    self.delay += u16::from_le_bytes([byte1, byte2]) as isize;
                    self.state = TapeState::PulseSequence { pulses_left };
                }
                TzxBlockId::PureDataBlock => {
                    let first_byte = self
                        .next_block_byte()?
                        .ok_or(TapeLoadError::InvalidTzxFile)?;
                    self.curr_byte = first_byte;
                    // Seem to need this flip for the block to load correctly.
                    self.curr_bit = !self.curr_bit;
                    self.state = TapeState::NextBit { mask: 0x80 };
                }
                TzxBlockId::PauseOrSilence => {
                    let byte1 = self
                        .next_block_byte()?
                        .ok_or(TapeLoadError::InvalidTzxFile)?;
                    let byte2 = self
                        .next_block_byte()?
                        .ok_or(TapeLoadError::InvalidTzxFile)?;

                    let length = u16::from_le_bytes([byte1, byte2]) as usize;
                    println!("\tPause/Silence length: {}ms", length);
                    // Finish off previous edge first
                    self.delay += 3_500;
                    // Post that play "silence" for specified length
                    self.state = TapeState::Silence { length };
                }
                TzxBlockId::LoopEnd => {
                    self.delay = 0;
                    self.state = TapeState::Play;
                }
                TzxBlockId::GroupStart => {
                    self.delay = 0;
                    self.state = TapeState::Play;
                }
                TzxBlockId::GroupEnd => {
                    self.delay = 0;
                    self.state = TapeState::Play;
                }
                TzxBlockId::Unknown | TzxBlockId::StopIf48k => {
                    println!("\tSkipping block");
                    self.state = TapeState::Play;
                }
                _ => {
                    //println!("\tSkipping block {:?}", block_id);
                    // Skip all bytes in the block
                    while self.next_block_byte()?.is_some() {}
                    self.state = TapeState::Play;
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
