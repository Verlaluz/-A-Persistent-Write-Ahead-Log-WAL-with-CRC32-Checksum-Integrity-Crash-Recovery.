use crc32fast::Hasher;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The binary frame header format:
/// [4 bytes: u32 payload length] + [4 bytes: u32 CRC32 checksum] = 8 bytes total
const HEADER_BYTE_SIZE: usize = 8;

pub struct WriteAheadLogManager {
    log_file_path: PathBuf,
    active_write_file_handle: File,
}

impl WriteAheadLogManager {
    pub fn open_or_create(target_file_path: &Path) -> Result<Self, String> {
        let opened_file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(target_file_path)
            .map_err(|filesystem_error| format!("Failed to open log file: {}", filesystem_error))?;

        Ok(WriteAheadLogManager {
            log_file_path: target_file_path.to_path_buf(),
            active_write_file_handle: opened_file,
        })
    }

    /// Appends a new binary transaction record to the log
    pub fn append_transaction_record(&mut self, transaction_payload_bytes: &[u8]) -> Result<u64, String> {
        let payload_length_u32: u32 = transaction_payload_bytes.len() as u32;

        // Compute the CRC32 checksum of the payload to catch bit flips or corrupted writes
        let mut checksum_calculator = Hasher::new();
        checksum_calculator.update(transaction_payload_bytes);
        let computed_checksum_u32: u32 = checksum_calculator.finalize();

        // Prepare the 8-byte binary header (little-endian byte order)
        let mut record_frame_bytes: Vec<u8> = Vec::with_capacity(HEADER_BYTE_SIZE + transaction_payload_bytes.len());
        record_frame_bytes.extend_from_slice(&payload_length_u32.to_le_bytes());
        record_frame_bytes.extend_from_slice(&computed_checksum_u32.to_le_bytes());
        record_frame_bytes.extend_from_slice(transaction_payload_bytes);

        // Write to disk and force synchronization
        self.active_write_file_handle
            .write_all(&record_frame_bytes)
            .map_err(|write_error| format!("Failed to write transaction frame: {}", write_error))?;

        // Flushes OS page cache buffers to physical storage (equivalent to fsync in C)
        self.active_write_file_handle
            .flush()
            .map_err(|flush_error| format!("Failed to flush log to disk: {}", flush_error))?;

        let current_log_byte_size = self
            .active_write_file_handle
            .seek(SeekFrom::End(0))
            .map_err(|seek_error| format!("Failed to inspect file offset: {}", seek_error))?;

        Ok(current_log_byte_size)
    }

    /// Crash Recovery Engine: Scans the log, validates checksums, and restores all valid records
    pub fn recover_and_replay_valid_records(&self) -> Result<Vec<Vec<u8>>, String> {
        let mut read_file_handle = File::open(&self.log_file_path)
            .map_err(|open_error| format!("Failed to open file for recovery: {}", open_error))?;

        let mut recovered_transactions_list: Vec<Vec<u8>> = Vec::new();
        let mut current_reading_byte_offset: u64 = 0;

        loop {
            let mut header_buffer = [0u8; HEADER_BYTE_SIZE];
            let read_bytes_count = read_file_handle
                .read(&mut header_buffer)
                .map_err(|read_error| format!("Failed reading header: {}", read_error))?;

            // Reached clean End-Of-File
            if read_bytes_count == 0 {
                break;
            }

            // Incomplete header detected (system lost power mid-header write)
            if read_bytes_count < HEADER_BYTE_SIZE {
                println!(
                    "⚠️  Recovery Warning: Incomplete header at offset {} (partial write detected). Halting recovery.",
                    current_reading_byte_offset
                );
                break;
            }

            let expected_payload_length = u32::from_le_bytes([
                header_buffer[0],
                header_buffer[1],
                header_buffer[2],
                header_buffer[3],
            ]) as usize;

            let expected_payload_checksum = u32::from_le_bytes([
                header_buffer[4],
                header_buffer[5],
                header_buffer[6],
                header_buffer[7],
            ]);

            // Allocate a buffer to hold the actual payload
            let mut payload_data_buffer = vec![0u8; expected_payload_length];
            let payload_read_count = read_file_handle
                .read(&mut payload_data_buffer)
                .map_err(|read_error| format!("Failed reading payload: {}", read_error))?;

            // Incomplete payload detected (system crashed mid-data write)
            if payload_read_count < expected_payload_length {
                println!(
                    "⚠️  Recovery Warning: Incomplete payload at offset {}. Expected {} bytes, found {}. Halting recovery.",
                    current_reading_byte_offset, expected_payload_length, payload_read_count
                );
                break;
            }

            // Verify payload integrity using CRC32
            let mut verification_hasher = Hasher::new();
            verification_hasher.update(&payload_data_buffer);
            let actual_calculated_checksum = verification_hasher.finalize();

            if actual_calculated_checksum != expected_payload_checksum {
                println!(
                    "❌ Corruption Detected at offset {}! Expected checksum {}, got {}. Halting.",
                    current_reading_byte_offset, expected_payload_checksum, actual_calculated_checksum
                );
                break;
            }

            // Record is 100% valid and verified
            current_reading_byte_offset += (HEADER_BYTE_SIZE + expected_payload_length) as u64;
            recovered_transactions_list.push(payload_data_buffer);
        }

        Ok(recovered_transactions_list)
    }
}

fn main() {
    let storage_file_path = Path::new("transaction_ledger.wal");

    // Clean up any old test run file
    if storage_file_path.exists() {
        let _ = std::fs::remove_file(storage_file_path);
    }

    println!("=== PHASE 1: NORMAL WRITING TO LOG ===");
    {
        let mut primary_wal_engine = WriteAheadLogManager::open_or_create(storage_file_path).unwrap();

        let transaction_batch = vec![
            "TX_1001: USER_ALICE DEPOSIT 2500 USD",
            "TX_1002: USER_BOB TRANSFER 500 USD TO USER_CHARLIE",
            "TX_1003: USER_CHARLIE PURCHASE 1 BTC",
        ];

        for single_transaction_text in transaction_batch {
            let offset_result = primary_wal_engine
                .append_transaction_record(single_transaction_text.as_bytes())
                .unwrap();
            println!("  💾 Committed '{}' (WAL File Size: {} bytes)", single_transaction_text, offset_result);
        }
    } // File handle closes here (simulating application shutdown)

    println!("\n=== PHASE 2: SIMULATING UNEXPECTED CRASH / POWER OUTAGE ===");
    {
        // Append raw corrupt bytes to simulate a power failure while writing a 4th transaction
        let mut corrupted_append_handle = OpenOptions::new()
            .append(true)
            .open(storage_file_path)
            .unwrap();

        // 4 bytes of length saying 50 bytes exist, followed by random junk that fails CRC
        let corrupted_partial_frame = [50u8, 0, 0, 0, 99, 99, 99, 99, 1, 2, 3, 4];
        corrupted_append_handle.write_all(&corrupted_partial_frame).unwrap();
        corrupted_append_handle.flush().unwrap();
        println!("  💥 Injected partial/corrupted write at the tail of the WAL file!");
    }

    println!("\n=== PHASE 3: CRASH RECOVERY REPLAY ===");
    {
        let recovery_wal_engine = WriteAheadLogManager::open_or_create(storage_file_path).unwrap();
        let restored_transactions = recovery_wal_engine.recover_and_replay_valid_records().unwrap();

        println!("\n✅ Successfully restored {} valid transactions:", restored_transactions.len());
        for (index_counter, single_recovered_bytes) in restored_transactions.iter().enumerate() {
            let decoded_text_string = String::from_utf8_lossy(single_recovered_bytes);
            println!("  [{}] {}", index_counter + 1, decoded_text_string);
        }
    }
}