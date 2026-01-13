//! Clock synchronization for screen streaming.
//!
//! Provides lightweight NTP-like clock synchronization between viewer and host
//! to enable accurate end-to-end latency measurement.
//!
//! ## Protocol
//!
//! 1. Viewer sends `TimeSyncRequest` with local timestamp
//! 2. Host immediately responds with `TimeSyncResponse`
//! 3. Repeat 5 times, use median RTT for offset calculation
//!
//! ## Offset Calculation
//!
//! ```text
//! RTT = (response_received - client_sent) - (server_send - server_receive)
//! Offset = server_receive + (RTT/2) - response_received
//! ```
//!
//! After sync, viewer can convert host timestamps to local time:
//! `local_time = host_time - offset`

/// Number of sync exchanges to perform for accurate offset
const SYNC_EXCHANGES: usize = 5;

/// Clock synchronization state.
///
/// Tracks the offset between local and remote clocks to enable
/// accurate latency measurement.
#[derive(Debug, Clone)]
pub struct ClockSync {
    /// Calculated offset in milliseconds: host_time = local_time + offset
    /// Positive means host clock is ahead of local clock
    offset_ms: i64,
    /// RTT samples from sync exchanges (in ms)
    rtt_samples: Vec<u32>,
    /// Offset samples for median calculation
    offset_samples: Vec<i64>,
    /// Whether initial sync is complete
    synced: bool,
    /// Sequence counter for sync requests
    next_sequence: u8,
}

impl Default for ClockSync {
    fn default() -> Self {
        Self::new()
    }
}

impl ClockSync {
    /// Create a new clock sync state.
    pub fn new() -> Self {
        Self {
            offset_ms: 0,
            rtt_samples: Vec::with_capacity(SYNC_EXCHANGES),
            offset_samples: Vec::with_capacity(SYNC_EXCHANGES),
            synced: false,
            next_sequence: 0,
        }
    }

    /// Get the next sequence number for a sync request.
    pub fn next_request_sequence(&mut self) -> u8 {
        let seq = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        seq
    }

    /// Check if sync is complete.
    pub fn is_synced(&self) -> bool {
        self.synced
    }

    /// Check if we need more sync exchanges.
    pub fn needs_more_samples(&self) -> bool {
        self.rtt_samples.len() < SYNC_EXCHANGES
    }

    /// Process a sync response.
    ///
    /// # Arguments
    /// * `client_time` - Original client timestamp from request (Unix millis)
    /// * `server_receive_time` - Server's time when it received request (Unix millis)
    /// * `server_send_time` - Server's time when it sent response (Unix millis)
    /// * `response_received_time` - Local time when response was received (Unix millis)
    ///
    /// # Returns
    /// `true` if sync is now complete (enough samples collected)
    pub fn process_response(
        &mut self,
        client_time: i64,
        server_receive_time: i64,
        server_send_time: i64,
        response_received_time: i64,
    ) -> bool {
        // Calculate RTT, accounting for server processing time
        // RTT = (response_received - client_sent) - (server_send - server_receive)
        let server_processing = server_send_time - server_receive_time;
        let rtt = (response_received_time - client_time) - server_processing;
        let rtt_ms = rtt.max(0) as u32;

        self.rtt_samples.push(rtt_ms);

        // Calculate offset for this sample
        // We assume the request took RTT/2 to reach the server
        // So: server_receive_time = client_time + one_way_delay
        // one_way_delay ≈ RTT/2
        // offset = server_time - local_time
        // At server_receive_time, local clock was at: client_time + RTT/2
        // So offset = server_receive_time - (client_time + RTT/2)
        let offset = server_receive_time - client_time - (rtt / 2);
        self.offset_samples.push(offset);

        tracing::debug!(
            "Clock sync sample {}: RTT={}ms, offset={}ms",
            self.rtt_samples.len(),
            rtt_ms,
            offset
        );

        // Check if we have enough samples
        if self.rtt_samples.len() >= SYNC_EXCHANGES {
            self.finalize_sync();
            true
        } else {
            false
        }
    }

    /// Finalize sync using median values.
    fn finalize_sync(&mut self) {
        // Use median RTT (more robust than mean)
        let mut sorted_rtt = self.rtt_samples.clone();
        sorted_rtt.sort();
        let median_rtt = sorted_rtt[sorted_rtt.len() / 2];

        // Use median offset
        let mut sorted_offset = self.offset_samples.clone();
        sorted_offset.sort();
        let median_offset = sorted_offset[sorted_offset.len() / 2];

        self.offset_ms = median_offset;
        self.synced = true;

        tracing::info!(
            "Clock sync complete: offset={}ms, median_rtt={}ms",
            self.offset_ms,
            median_rtt
        );
    }

    /// Convert a host timestamp to local time.
    ///
    /// # Arguments
    /// * `host_ms` - Timestamp from host (Unix millis)
    ///
    /// # Returns
    /// Estimated local time when that moment occurred
    pub fn to_local_time(&self, host_ms: i64) -> i64 {
        host_ms - self.offset_ms
    }

    /// Convert local time to estimated host time.
    ///
    /// # Arguments
    /// * `local_ms` - Local timestamp (Unix millis)
    ///
    /// # Returns
    /// Estimated host time at that moment
    pub fn to_host_time(&self, local_ms: i64) -> i64 {
        local_ms + self.offset_ms
    }

    /// Get the clock offset in milliseconds.
    ///
    /// Positive means host clock is ahead of local clock.
    pub fn offset_ms(&self) -> i64 {
        self.offset_ms
    }

    /// Get average RTT from sync exchanges in milliseconds.
    pub fn avg_rtt_ms(&self) -> u32 {
        if self.rtt_samples.is_empty() {
            0
        } else {
            self.rtt_samples.iter().sum::<u32>() / self.rtt_samples.len() as u32
        }
    }

    /// Get minimum RTT from sync exchanges in milliseconds.
    pub fn min_rtt_ms(&self) -> u32 {
        self.rtt_samples.iter().copied().min().unwrap_or(0)
    }

    /// Reset sync state (e.g., for re-sync).
    pub fn reset(&mut self) {
        self.offset_ms = 0;
        self.rtt_samples.clear();
        self.offset_samples.clear();
        self.synced = false;
        self.next_sequence = 0;
    }

    /// Force an offset value (for testing or manual override).
    #[cfg(test)]
    pub fn set_offset(&mut self, offset_ms: i64) {
        self.offset_ms = offset_ms;
        self.synced = true;
    }

    /// Apply externally computed clock sync values.
    ///
    /// This is used when clock sync is performed externally (e.g., by the transfer layer)
    /// and the offset/RTT values need to be passed to the viewer.
    ///
    /// # Arguments
    /// * `offset_ms` - Clock offset in milliseconds (host_time = local_time + offset)
    /// * `rtt_ms` - Round-trip time in milliseconds
    pub fn apply_sync(&mut self, offset_ms: i64, rtt_ms: u32) {
        self.offset_ms = offset_ms;
        self.rtt_samples.clear();
        self.rtt_samples.push(rtt_ms);
        self.synced = true;
        tracing::debug!(
            "Applied external clock sync: offset={}ms, rtt={}ms",
            offset_ms,
            rtt_ms
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clock_sync_basic() {
        let mut sync = ClockSync::new();

        assert!(!sync.is_synced());
        assert!(sync.needs_more_samples());

        // Simulate 5 sync exchanges with 50ms RTT and 100ms offset
        // The math: if server clock is 100ms ahead of client:
        // - Client sends at t=1000 (client time)
        // - Server receives at t=1000+25+100=1125 (server time, 25ms transit + 100ms offset)
        // - Server sends at t=1126 (1ms processing)
        // - Client receives at t=1000+50+1=1051 (client time, 50ms RTT + 1ms processing)
        for i in 0..5 {
            let client_time: i64 = 1000 + i * 100;
            let server_receive_time: i64 = client_time + 25 + 100; // 25ms transit + 100ms offset
            let server_send_time: i64 = server_receive_time + 1; // 1ms processing
            let response_received_time: i64 = client_time + 50 + 1; // 50ms RTT + 1ms processing

            sync.process_response(
                client_time,
                server_receive_time,
                server_send_time,
                response_received_time,
            );
        }

        assert!(sync.is_synced());
        assert!(!sync.needs_more_samples());

        // Offset should be approximately 100ms (allow 30ms tolerance due to algorithm)
        let offset = sync.offset_ms();
        assert!(
            (offset - 100).abs() < 30,
            "Expected offset ~100ms, got {}ms",
            offset
        );
    }

    #[test]
    fn test_time_conversion() {
        let mut sync = ClockSync::new();
        sync.set_offset(100); // Host is 100ms ahead

        // Host time 1000 should be local time 900
        assert_eq!(sync.to_local_time(1000), 900);

        // Local time 900 should be host time 1000
        assert_eq!(sync.to_host_time(900), 1000);
    }

    #[test]
    fn test_rtt_stats() {
        let mut sync = ClockSync::new();

        // Add samples with different RTTs
        for (i, rtt) in [50, 60, 40, 55, 45].iter().enumerate() {
            let client_time = 1000 + i as i64 * 100;
            let server_receive_time = client_time + 100 + rtt / 2;
            let server_send_time = server_receive_time + 1;
            let response_received_time = client_time + *rtt + 1;

            sync.process_response(
                client_time,
                server_receive_time,
                server_send_time,
                response_received_time,
            );
        }

        assert!(sync.is_synced());
        assert_eq!(sync.min_rtt_ms(), 40);
        assert_eq!(sync.avg_rtt_ms(), 50);
    }
}
