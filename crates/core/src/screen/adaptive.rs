//! Adaptive bitrate and frame sync for screen streaming.
//!
//! This module provides:
//! - `AdaptiveBitrate`: GCC-inspired bitrate controller that adjusts based on network conditions
//! - `PacketLossTracker`: Tracks sequence gaps to calculate packet loss rate
//! - `FrameSyncState`: Manages frame timing and decides when to drop frames
//!
//! ## Adaptive Bitrate Algorithm
//!
//! Based on Google Congestion Control (GCC):
//! - Decrease 15% on packet loss > 2%
//! - Hold steady when RTT is increasing
//! - Increase 5% when stable (loss < 1%, RTT stable)
//!
//! ## Frame Sync
//!
//! Frames are evaluated based on end-to-end latency:
//! - Display normally if latency < 2 * target
//! - Drop frame if latency > max_latency
//! - Request quality reduction if consistently behind
//! - Request keyframe + flush if severely behind

use std::collections::VecDeque;

use crate::iroh::protocol::ScreenQuality;

// ============================================================================
// Adaptive Bitrate Controller
// ============================================================================

/// Bitrate adjustment state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BitrateState {
    /// Probing for more bandwidth
    Increase,
    /// Holding steady
    #[default]
    Hold,
    /// Reducing due to congestion
    Decrease,
}

/// Google Congestion Control inspired adaptive bitrate controller.
///
/// Adjusts bitrate based on RTT trends and packet loss.
#[derive(Debug)]
pub struct AdaptiveBitrate {
    /// Minimum bitrate floor (kbps)
    min_bitrate_kbps: u32,
    /// Maximum bitrate ceiling (kbps)
    max_bitrate_kbps: u32,
    /// Current target bitrate (kbps)
    current_bitrate_kbps: u32,
    /// RTT history (last 10 samples, in ms)
    rtt_history: VecDeque<u32>,
    /// Packet loss history (last 10 samples, 0.0-1.0)
    loss_history: VecDeque<f32>,
    /// Current state
    state: BitrateState,
    /// Frames since last adjustment
    frames_since_adjust: u32,
    /// Minimum frames between adjustments
    adjust_interval: u32,
}

impl AdaptiveBitrate {
    /// Create a new adaptive bitrate controller.
    ///
    /// # Arguments
    /// * `min_kbps` - Minimum bitrate (default: 500 kbps)
    /// * `max_kbps` - Maximum bitrate (default: 20,000 kbps)
    /// * `initial_kbps` - Starting bitrate
    pub fn new(min_kbps: u32, max_kbps: u32, initial_kbps: u32) -> Self {
        Self {
            min_bitrate_kbps: min_kbps,
            max_bitrate_kbps: max_kbps,
            current_bitrate_kbps: initial_kbps.clamp(min_kbps, max_kbps),
            rtt_history: VecDeque::with_capacity(10),
            loss_history: VecDeque::with_capacity(10),
            state: BitrateState::Hold,
            frames_since_adjust: 0,
            adjust_interval: 30, // ~0.5 sec at 60fps
        }
    }

    /// Create with default settings.
    pub fn with_defaults() -> Self {
        Self::new(500, 20_000, 5_000)
    }

    /// Update with new measurements, returns new bitrate if changed.
    ///
    /// # Arguments
    /// * `rtt_ms` - Current RTT in milliseconds
    /// * `packet_loss` - Current packet loss rate (0.0-1.0)
    ///
    /// # Returns
    /// `Some(new_bitrate)` if bitrate should change, `None` otherwise
    pub fn update(&mut self, rtt_ms: u32, packet_loss: f32) -> Option<u32> {
        // Record samples
        self.rtt_history.push_back(rtt_ms);
        if self.rtt_history.len() > 10 {
            self.rtt_history.pop_front();
        }

        self.loss_history.push_back(packet_loss);
        if self.loss_history.len() > 10 {
            self.loss_history.pop_front();
        }

        self.frames_since_adjust += 1;

        // Don't adjust too frequently
        if self.frames_since_adjust < self.adjust_interval {
            return None;
        }

        let avg_loss = self.avg_loss();
        let rtt_trend = self.rtt_trend();

        let new_bitrate = if avg_loss > 0.02 {
            // >2% packet loss: decrease by 15%
            self.state = BitrateState::Decrease;
            (self.current_bitrate_kbps as f32 * 0.85) as u32
        } else if rtt_trend > 0.1 {
            // RTT increasing significantly: hold steady
            self.state = BitrateState::Hold;
            self.current_bitrate_kbps
        } else if avg_loss < 0.01 && rtt_trend < 0.05 {
            // Stable conditions: increase by 5%
            self.state = BitrateState::Increase;
            (self.current_bitrate_kbps as f32 * 1.05) as u32
        } else {
            self.state = BitrateState::Hold;
            self.current_bitrate_kbps
        };

        let clamped = new_bitrate.clamp(self.min_bitrate_kbps, self.max_bitrate_kbps);

        if clamped != self.current_bitrate_kbps {
            let old = self.current_bitrate_kbps;
            self.current_bitrate_kbps = clamped;
            self.frames_since_adjust = 0;

            tracing::debug!(
                "Adaptive bitrate: {} -> {} kbps (loss={:.1}%, rtt_trend={:.2})",
                old,
                clamped,
                avg_loss * 100.0,
                rtt_trend
            );

            Some(clamped)
        } else {
            None
        }
    }

    /// Force an immediate decrease (called on severe congestion).
    ///
    /// Halves the current bitrate down to minimum.
    pub fn force_decrease(&mut self) -> u32 {
        let old = self.current_bitrate_kbps;
        self.current_bitrate_kbps =
            (self.current_bitrate_kbps / 2).max(self.min_bitrate_kbps);
        self.state = BitrateState::Decrease;
        self.frames_since_adjust = 0;

        tracing::warn!(
            "Forced bitrate decrease: {} -> {} kbps",
            old,
            self.current_bitrate_kbps
        );

        self.current_bitrate_kbps
    }

    /// Get current target bitrate in kbps.
    pub fn current_bitrate(&self) -> u32 {
        self.current_bitrate_kbps
    }

    /// Get current state.
    pub fn state(&self) -> BitrateState {
        self.state
    }

    /// Calculate average packet loss over history.
    fn avg_loss(&self) -> f32 {
        if self.loss_history.is_empty() {
            0.0
        } else {
            self.loss_history.iter().sum::<f32>() / self.loss_history.len() as f32
        }
    }

    /// Calculate RTT trend (positive = increasing, negative = decreasing).
    ///
    /// Returns relative change: (second_half_avg - first_half_avg) / first_half_avg
    fn rtt_trend(&self) -> f32 {
        if self.rtt_history.len() < 4 {
            return 0.0;
        }

        let mid = self.rtt_history.len() / 2;
        let first_half: f32 = self.rtt_history.iter().take(mid).map(|&x| x as f32).sum();
        let second_half: f32 = self.rtt_history.iter().skip(mid).map(|&x| x as f32).sum();

        let first_avg = first_half / mid as f32;
        let second_avg = second_half / (self.rtt_history.len() - mid) as f32;

        if first_avg > 0.0 {
            (second_avg - first_avg) / first_avg
        } else {
            0.0
        }
    }

    /// Reset the controller state.
    pub fn reset(&mut self, initial_bitrate: u32) {
        self.current_bitrate_kbps = initial_bitrate.clamp(self.min_bitrate_kbps, self.max_bitrate_kbps);
        self.rtt_history.clear();
        self.loss_history.clear();
        self.state = BitrateState::Hold;
        self.frames_since_adjust = 0;
    }
}

// ============================================================================
// Packet Loss Tracker
// ============================================================================

/// Tracks packet loss by detecting sequence gaps.
#[derive(Debug)]
pub struct PacketLossTracker {
    /// Expected next sequence number
    expected_seq: u64,
    /// Packets received in current window
    received: u32,
    /// Packets lost (gaps) in current window
    lost: u32,
    /// Window size for calculation (number of packets)
    window_size: u32,
    /// Whether we've received the first packet
    initialized: bool,
}

impl PacketLossTracker {
    /// Create a new packet loss tracker.
    ///
    /// # Arguments
    /// * `window_size` - Number of packets per measurement window (e.g., 60 for 1 second at 60fps)
    pub fn new(window_size: u32) -> Self {
        Self {
            expected_seq: 0,
            received: 0,
            lost: 0,
            window_size,
            initialized: false,
        }
    }

    /// Record receipt of a packet with given sequence number.
    ///
    /// # Returns
    /// Current loss rate (0.0-1.0). Rate is reset after each window.
    pub fn record(&mut self, sequence: u64) -> f32 {
        if !self.initialized {
            self.expected_seq = sequence + 1;
            self.received = 1;
            self.initialized = true;
            return 0.0;
        }

        // Check for gaps (lost packets)
        if sequence > self.expected_seq {
            let gap = (sequence - self.expected_seq) as u32;
            self.lost += gap;
            tracing::trace!(
                "Packet loss detected: expected seq {}, got {}, gap={}",
                self.expected_seq,
                sequence,
                gap
            );
        } else if sequence < self.expected_seq {
            // Out of order or duplicate - ignore for now
            tracing::trace!(
                "Out of order packet: expected {}, got {}",
                self.expected_seq,
                sequence
            );
        }

        self.received += 1;
        self.expected_seq = sequence + 1;

        // Calculate and potentially reset
        let total = self.received + self.lost;
        let loss_rate = self.lost as f32 / total as f32;

        if total >= self.window_size {
            // Window complete, reset for next window
            if self.lost > 0 {
                tracing::debug!(
                    "Packet loss window: {}/{} lost ({:.1}%)",
                    self.lost,
                    total,
                    loss_rate * 100.0
                );
            }
            self.received = 0;
            self.lost = 0;
        }

        loss_rate
    }

    /// Get current loss rate without resetting.
    pub fn current_rate(&self) -> f32 {
        let total = self.received + self.lost;
        if total == 0 {
            0.0
        } else {
            self.lost as f32 / total as f32
        }
    }

    /// Reset the tracker.
    pub fn reset(&mut self) {
        self.expected_seq = 0;
        self.received = 0;
        self.lost = 0;
        self.initialized = false;
    }
}

// ============================================================================
// Frame Sync State
// ============================================================================

/// Action to take for a received frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameAction {
    /// Display the frame normally
    Display,
    /// Drop the frame (too old)
    Drop,
    /// Display but request quality reduction from host
    RequestQualityReduction,
    /// Drop all buffered frames and request a keyframe
    RequestKeyframeFlush,
}

/// Frame timing and synchronization state.
///
/// Tracks latency and decides when to drop frames or request adjustments.
#[derive(Debug)]
pub struct FrameSyncState {
    /// Target latency we're aiming for (ms)
    target_latency_ms: u32,
    /// Max latency before frame dropping (ms)
    max_latency_ms: u32,
    /// Latency samples (rolling window)
    latency_window: VecDeque<i64>,
    /// Window size for latency averaging
    latency_window_size: usize,
    /// Consecutive frames above target latency
    frames_behind: u32,
    /// Whether we're in "catching up" mode
    catching_up: bool,
    /// Threshold for triggering quality reduction
    quality_reduction_threshold: u32,
    /// Threshold for triggering keyframe flush
    flush_threshold: u32,
}

impl FrameSyncState {
    /// Create a new frame sync state.
    ///
    /// # Arguments
    /// * `target_latency_ms` - Target end-to-end latency (default: 100ms)
    /// * `max_latency_ms` - Maximum acceptable latency before dropping (default: 500ms)
    pub fn new(target_latency_ms: u32, max_latency_ms: u32) -> Self {
        Self {
            target_latency_ms,
            max_latency_ms,
            latency_window: VecDeque::with_capacity(30),
            latency_window_size: 30,
            frames_behind: 0,
            catching_up: false,
            quality_reduction_threshold: 10,  // Request reduction after 10 late frames
            flush_threshold: 20,               // Flush after 20 late frames
        }
    }

    /// Create with default settings.
    pub fn with_defaults() -> Self {
        Self::new(100, 500)
    }

    /// Record a latency sample and determine action.
    ///
    /// # Arguments
    /// * `latency_ms` - End-to-end latency for this frame (ms)
    ///
    /// # Returns
    /// Action to take for this frame
    pub fn record_latency(&mut self, latency_ms: i64) -> FrameAction {
        // Maintain rolling window
        self.latency_window.push_back(latency_ms);
        if self.latency_window.len() > self.latency_window_size {
            self.latency_window.pop_front();
        }

        // Check if frame is too old
        if latency_ms > self.max_latency_ms as i64 {
            self.frames_behind += 1;
            tracing::trace!(
                "Frame too old: latency={}ms > max={}ms, behind={}",
                latency_ms,
                self.max_latency_ms,
                self.frames_behind
            );

            if self.frames_behind > 5 {
                self.catching_up = true;
            }

            // Check if we should flush
            if self.catching_up && self.frames_behind > self.flush_threshold {
                tracing::warn!(
                    "Severely behind ({}ms, {} frames), requesting keyframe flush",
                    latency_ms,
                    self.frames_behind
                );
                return FrameAction::RequestKeyframeFlush;
            }

            return FrameAction::Drop;
        }

        // Check if falling behind (latency > 2x target)
        let behind_threshold = (self.target_latency_ms as i64) * 2;
        if latency_ms > behind_threshold {
            self.frames_behind += 1;

            if self.frames_behind > self.quality_reduction_threshold && !self.catching_up {
                tracing::debug!(
                    "Falling behind ({}ms > {}ms target, {} frames), requesting quality reduction",
                    latency_ms,
                    self.target_latency_ms,
                    self.frames_behind
                );
                return FrameAction::RequestQualityReduction;
            }
        } else {
            // Latency is acceptable
            self.frames_behind = 0;
            self.catching_up = false;
        }

        FrameAction::Display
    }

    /// Evaluate a frame's captured_at timestamp.
    ///
    /// # Arguments
    /// * `captured_at_ms` - Host's capture timestamp (Unix millis, already adjusted for clock offset)
    /// * `now_ms` - Current local time (Unix millis)
    ///
    /// # Returns
    /// Action to take for this frame
    pub fn evaluate_frame(&mut self, captured_at_ms: i64, now_ms: i64) -> FrameAction {
        let latency = (now_ms - captured_at_ms).max(0);
        self.record_latency(latency)
    }

    /// Get average latency over window.
    pub fn avg_latency_ms(&self) -> u32 {
        if self.latency_window.is_empty() {
            0
        } else {
            let sum: i64 = self.latency_window.iter().sum();
            (sum / self.latency_window.len() as i64).max(0) as u32
        }
    }

    /// Get current latency (most recent sample).
    pub fn current_latency_ms(&self) -> u32 {
        self.latency_window.back().copied().unwrap_or(0).max(0) as u32
    }

    /// Check if we're currently catching up.
    pub fn is_catching_up(&self) -> bool {
        self.catching_up
    }

    /// Get number of consecutive late frames.
    pub fn frames_behind(&self) -> u32 {
        self.frames_behind
    }

    /// Reset after a keyframe flush.
    pub fn reset_after_flush(&mut self) {
        self.frames_behind = 0;
        self.catching_up = false;
        self.latency_window.clear();
        tracing::debug!("Frame sync state reset after flush");
    }

    /// Update target latency.
    pub fn set_target_latency(&mut self, target_ms: u32) {
        self.target_latency_ms = target_ms;
    }

    /// Update max latency.
    pub fn set_max_latency(&mut self, max_ms: u32) {
        self.max_latency_ms = max_ms;
    }

    /// Convert quality reduction request to quality hint.
    pub fn suggest_quality(&self, current: ScreenQuality) -> ScreenQuality {
        match current {
            ScreenQuality::Quality => ScreenQuality::Balanced,
            ScreenQuality::Balanced => ScreenQuality::Fast,
            ScreenQuality::Fast => ScreenQuality::Fast, // Already at lowest
            ScreenQuality::Auto => ScreenQuality::Fast, // Force to fast when behind
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adaptive_bitrate_stable() {
        let mut abr = AdaptiveBitrate::new(500, 20_000, 5_000);

        // Simulate stable conditions for multiple windows
        for _ in 0..100 {
            abr.update(50, 0.005); // 50ms RTT, 0.5% loss
        }

        // Should have increased somewhat
        assert!(abr.current_bitrate() > 5_000);
        assert_eq!(abr.state(), BitrateState::Increase);
    }

    #[test]
    fn test_adaptive_bitrate_congestion() {
        let mut abr = AdaptiveBitrate::new(500, 20_000, 10_000);

        // Simulate packet loss
        for _ in 0..40 {
            abr.update(100, 0.05); // 5% loss
        }

        // Should have decreased
        assert!(abr.current_bitrate() < 10_000);
        assert_eq!(abr.state(), BitrateState::Decrease);
    }

    #[test]
    fn test_adaptive_bitrate_force_decrease() {
        let mut abr = AdaptiveBitrate::new(500, 20_000, 10_000);

        let new_rate = abr.force_decrease();
        assert_eq!(new_rate, 5_000);
        assert_eq!(abr.current_bitrate(), 5_000);

        let new_rate = abr.force_decrease();
        assert_eq!(new_rate, 2_500);
    }

    #[test]
    fn test_packet_loss_tracker() {
        let mut tracker = PacketLossTracker::new(10);

        // Sequential packets, no loss
        for i in 0..10 {
            let loss = tracker.record(i);
            assert_eq!(loss, 0.0);
        }

        // Gap of 2 packets
        tracker.record(12); // Expected 10, got 12
        let loss = tracker.current_rate();
        assert!(loss > 0.0);
    }

    #[test]
    fn test_frame_sync_normal() {
        let mut sync = FrameSyncState::new(100, 500);

        // Normal latency
        let action = sync.record_latency(80);
        assert_eq!(action, FrameAction::Display);
        assert_eq!(sync.frames_behind(), 0);
    }

    #[test]
    fn test_frame_sync_high_latency() {
        let mut sync = FrameSyncState::new(100, 500);

        // High but acceptable latency
        for _ in 0..15 {
            let action = sync.record_latency(250);
            // After threshold, should request quality reduction
            if sync.frames_behind() > 10 {
                assert_eq!(action, FrameAction::RequestQualityReduction);
            }
        }
    }

    #[test]
    fn test_frame_sync_drop() {
        let mut sync = FrameSyncState::new(100, 500);

        // Way too high latency
        let action = sync.record_latency(600);
        assert_eq!(action, FrameAction::Drop);
    }

    #[test]
    fn test_frame_sync_flush() {
        let mut sync = FrameSyncState::new(100, 500);

        // Many late frames
        for i in 0..25 {
            let action = sync.record_latency(600);
            if i >= 20 {
                assert_eq!(action, FrameAction::RequestKeyframeFlush);
            }
        }
    }
}
