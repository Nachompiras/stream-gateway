use bytes::{Bytes, BytesMut, BufMut};
use log::{debug, warn, trace};
use std::collections::HashSet;
use once_cell::sync::Lazy;

/// MPEG-TS packet size in bytes
pub const TS_PACKET_SIZE: usize = 188;

/// Sync byte that starts every TS packet
const TS_SYNC_BYTE: u8 = 0x47;

/// PID for Program Association Table
const PAT_PID: u16 = 0x0000;

/// PID for null packets
#[allow(dead_code)]
const NULL_PID: u16 = 0x1FFF;

/// PID for Conditional Access Table
const CAT_PID: u16 = 0x0001;

/// Static null packet to avoid repeated allocations
static NULL_PACKET: Lazy<Bytes> = Lazy::new(|| {
    let mut packet = BytesMut::with_capacity(TS_PACKET_SIZE);
    packet.put_u8(TS_SYNC_BYTE);
    packet.put_u8(0x1F); // PID high byte
    packet.put_u8(0xFF); // PID low byte (0x1FFF)
    packet.put_u8(0x10); // No adaptation field, payload present, counter = 0
    packet.resize(TS_PACKET_SIZE, 0xFF);
    packet.freeze()
});

/// Represents a single TS packet
#[derive(Debug, Clone)]
pub struct TsPacket {
    pub pid: u16,
    pub payload_unit_start: bool,
    pub data: Bytes,
}

impl TsPacket {
    /// Parse a TS packet from raw bytes
    #[inline(always)]
    pub fn parse(data: Bytes) -> Option<Self> {
        // Fast path: validate size and sync byte without logging
        if data.len() != TS_PACKET_SIZE || data[0] != TS_SYNC_BYTE {
            return None;
        }

        // Extract PID from bytes 1-2
        // Byte 1: | TEI | PUSI | TP | PID[12:8] |
        // Byte 2: | PID[7:0] |
        let pid = (((data[1] & 0x1F) as u16) << 8) | (data[2] as u16);

        let payload_unit_start = (data[1] & 0x40) != 0;

        Some(TsPacket {
            pid,
            payload_unit_start,
            data,
        })
    }
}

/// Information about a program in the stream
#[derive(Debug, Clone)]
pub struct ProgramInfo {
    pub program_number: u16,
    pub pmt_pid: u16,
    pub pids: HashSet<u16>, // All PIDs belonging to this program
}

/// Filters MPEG-TS packets to extract a single program (SPTS) from an MPTS
pub struct ProgramFilter {
    target_program: u16,
    program_info: Option<ProgramInfo>,
    pat_version: Option<u8>,
    pmt_version: Option<u8>,
    #[allow(dead_code)]
    pat_continuity: u8,
    #[allow(dead_code)]
    pmt_continuity: u8,
    #[allow(dead_code)]
    pat_cache: Option<Bytes>,
    #[allow(dead_code)]
    pmt_cache: Option<Bytes>,
    fill_with_nulls: bool,
    output_buffer: BytesMut,
}

impl ProgramFilter {
    /// Create a new program filter for the specified program number
    pub fn new(program_number: u16, fill_with_nulls: bool) -> Self {
        debug!("Creating ProgramFilter for program {}", program_number);
        Self {
            target_program: program_number,
            program_info: None,
            pat_version: None,
            pmt_version: None,
            pat_continuity: 0,
            pmt_continuity: 0,
            pat_cache: None,
            pmt_cache: None,
            fill_with_nulls,
            output_buffer: BytesMut::new(),
        }
    }

    /// Filter a batch of packets and return only those belonging to the target program
    pub fn filter_packets(&mut self, input: &Bytes) -> Bytes {
        // Clear buffer but keep capacity
        self.output_buffer.clear();

        // Process packets in chunks of TS_PACKET_SIZE
        let mut offset = 0;
        while offset + TS_PACKET_SIZE <= input.len() {
            let packet_data = input.slice(offset..offset + TS_PACKET_SIZE);

            if let Some(packet) = TsPacket::parse(packet_data) {
                let pid = packet.pid;

                // Inline should_include_packet logic for performance
                let should_include = if pid == PAT_PID || pid == CAT_PID {
                    true
                } else if let Some(ref info) = self.program_info {
                    pid == info.pmt_pid || info.pids.contains(&pid)
                } else {
                    false
                };

                if should_include {
                    // Process PAT/PMT packets to learn program structure
                    if pid == PAT_PID && packet.payload_unit_start {
                        self.process_pat(&packet.data);
                    } else if let Some(ref info) = self.program_info {
                        if pid == info.pmt_pid && packet.payload_unit_start {
                            self.process_pmt(&packet.data);
                        }
                    }

                    // Include packet in output
                    self.output_buffer.put(packet.data);
                } else if self.fill_with_nulls {
                    // Replace with null packet to maintain bitrate (zero-cost clone)
                    self.output_buffer.put(NULL_PACKET.clone());
                }
            } else if self.fill_with_nulls {
                // Invalid packet, replace with null
                self.output_buffer.put(NULL_PACKET.clone());
            }

            offset += TS_PACKET_SIZE;
        }

        // Split the buffer to avoid cloning
        self.output_buffer.split().freeze()
    }


    /// Process PAT (Program Association Table) to find our program's PMT PID
    fn process_pat(&mut self, data: &Bytes) {
        trace!("Processing PAT packet");

        // Skip TS header (4 bytes) and pointer field (1 byte for PUSI packets)
        let mut offset = 4;
        if data[1] & 0x40 != 0 { // payload_unit_start_indicator
            offset += 1 + data[4] as usize; // pointer_field
        }

        if offset + 8 > data.len() {
            warn!("PAT packet too short");
            return;
        }

        // Parse PSI header
        let table_id = data[offset];
        if table_id != 0x00 {
            return; // Not a PAT
        }

        let section_length = (((data[offset + 1] & 0x0F) as usize) << 8) | (data[offset + 2] as usize);
        let version = (data[offset + 5] >> 1) & 0x1F;

        // Check if this is a new version
        if let Some(current_version) = self.pat_version {
            if current_version == version {
                return; // Same version, skip
            }
        }

        debug!("New PAT version: {}", version);
        self.pat_version = Some(version);

        // Parse program associations
        // Skip PSI header (8 bytes) and CRC (4 bytes)
        let programs_start = offset + 8;
        let programs_end = offset + 3 + section_length - 4;

        for i in (programs_start..programs_end).step_by(4) {
            if i + 4 > data.len() {
                break;
            }

            let program_number = ((data[i] as u16) << 8) | (data[i + 1] as u16);
            let pid = (((data[i + 2] & 0x1F) as u16) << 8) | (data[i + 3] as u16);

            if program_number == self.target_program {
                debug!("Found target program {} with PMT PID {}", program_number, pid);

                // Initialize program info if not already present
                if self.program_info.is_none() {
                    self.program_info = Some(ProgramInfo {
                        program_number,
                        pmt_pid: pid,
                        pids: HashSet::new(),
                    });
                }

                break;
            }
        }
    }

    /// Process PMT (Program Map Table) to learn all PIDs for our program
    fn process_pmt(&mut self, data: &Bytes) {
        trace!("Processing PMT packet");

        // Skip TS header (4 bytes) and pointer field (1 byte for PUSI packets)
        let mut offset = 4;
        if data[1] & 0x40 != 0 { // payload_unit_start_indicator
            offset += 1 + data[4] as usize; // pointer_field
        }

        if offset + 12 > data.len() {
            warn!("PMT packet too short");
            return;
        }

        // Parse PSI header
        let table_id = data[offset];
        if table_id != 0x02 {
            return; // Not a PMT
        }

        let section_length = (((data[offset + 1] & 0x0F) as usize) << 8) | (data[offset + 2] as usize);
        let version = (data[offset + 5] >> 1) & 0x1F;

        // Check if this is a new version
        if let Some(current_version) = self.pmt_version {
            if current_version == version {
                return; // Same version, skip
            }
        }

        debug!("New PMT version: {}", version);
        self.pmt_version = Some(version);

        // Get PCR PID
        let pcr_pid = (((data[offset + 8] & 0x1F) as u16) << 8) | (data[offset + 9] as u16);

        // Get program info length
        let program_info_length = (((data[offset + 10] & 0x0F) as usize) << 8) | (data[offset + 11] as usize);

        // Parse elementary streams
        let mut stream_offset = offset + 12 + program_info_length;
        let section_end = offset + 3 + section_length - 4; // -4 for CRC

        let mut pids = HashSet::new();
        pids.insert(pcr_pid); // Include PCR PID

        while stream_offset + 5 <= section_end && stream_offset + 5 <= data.len() {
            let stream_type = data[stream_offset];
            let elementary_pid = (((data[stream_offset + 1] & 0x1F) as u16) << 8) | (data[stream_offset + 2] as u16);
            let es_info_length = (((data[stream_offset + 3] & 0x0F) as usize) << 8) | (data[stream_offset + 4] as usize);

            debug!("Found elementary stream: type={}, PID={}", stream_type, elementary_pid);
            pids.insert(elementary_pid);

            stream_offset += 5 + es_info_length;
        }

        // Update program info with discovered PIDs
        if let Some(ref mut info) = self.program_info {
            info.pids = pids;
            debug!("Program {} now tracking {} PIDs", info.program_number, info.pids.len());
        }
    }

    /// Get current program info if available
    #[allow(dead_code)]
    pub fn get_program_info(&self) -> Option<&ProgramInfo> {
        self.program_info.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ts_packet_parse() {
        let mut data = BytesMut::with_capacity(TS_PACKET_SIZE);
        data.put_u8(TS_SYNC_BYTE);
        data.put_u8(0x00); // PID high byte
        data.put_u8(0x00); // PID low byte (PAT)
        data.resize(TS_PACKET_SIZE, 0xFF);

        let packet = TsPacket::parse(data.freeze()).unwrap();
        assert_eq!(packet.pid, PAT_PID);
    } 

    #[test]
    fn test_program_filter_creation() {
        let filter = ProgramFilter::new(1, false);
        assert_eq!(filter.target_program, 1);
        assert!(filter.program_info.is_none());
    }
}
