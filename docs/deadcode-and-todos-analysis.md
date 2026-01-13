Comprehensive Report: #[allow(dead_code)] and TODO/Similar Markers
Executive Summary
This codebase is in pre-alpha development with extensive planning documentation. The dead code markers and TODOs represent a deliberate phased approach to development, where types and infrastructure are created before full implementation.

1. #[allow(dead_code)] Instances
1.1 Test Utilities (Intentionally Unused Until Needed)
File: tests/common/mod.rs

Line	Item	Purpose
12	FAIL_TIMEOUT	Shorter timeout for operations expected to fail quickly
19	init_test_logging()	Initialize tracing subscriber for test debugging
36	with_timeout()	Timeout wrapper for async test operations
Reason: These are test helpers available for use but not all tests need them. Standard practice for shared test modules.

1.2 Work-In-Progress (WIP) Features
File: gui/src/app.rs:126


#[allow(dead_code)] // WIP: Peer introduction feature not yet implemented
struct PendingIntroduction { ... }
Contains:

introduction_id - Unique introduction ID
network_id - Network where introduction happens
peer_to_introduce_id/name - Peer B being introduced
target_member_id/name - Target network member C
peer_b_accepted / peer_c_accepted - Acceptance tracking
started_at - When introduction started
Related Documentation: misc/croc-gui-v2-plan.md - Part Two covers peer networking and trust establishment.

1.3 Deprecated Functions
File: gui/src/app.rs:9060


/// NOTE: This function is deprecated - trust handshakes are now handled by the background listener.
#[allow(dead_code)]
async fn wait_for_trust_handshake(...) -> croh_core::Result<TrustedPeer>
Status: Trust handshakes were refactored to use a background listener pattern. This function is kept for reference/fallback but not actively used.

1.4 Future Croc Output Parsing Features
File: core/src/croc/output.rs

Line	Function	Purpose
170	detect_waiting()	Detect if croc is waiting for receiver
179	parse_file_info()	Parse file info from croc output
195	parse_size_string()	Convert human-readable size to bytes
Comment: "Will be used in future phases"

Related: misc/croc-gui-v2-plan.md Phase 0.2 describes the full croc output parsing specification.

1.5 Test Cache Clearing
File: core/src/croc/executable.rs:236


#[cfg(test)]
#[allow(dead_code)]
pub fn clear_cache() { ... }
Purpose: Clear cached croc path for testing different scenarios. Available but not used by current tests.

1.6 Database/Store Fields
File: core/src/chat/store.rs:18


pub struct ChatStore {
    #[allow(dead_code)]
    db: Db,
    ...
}
Reason: The db field is stored for reference but most operations use the specific Tree fields directly.

1.7 Platform-Specific Screen Capture Fields
Multiple files in the screen capture module have unused fields stored for future use:

screen/drm.rs:

Line	Field	Purpose
63	ConnectorInfo::handle	Connector handle (future operations)
66	ConnectorInfo::name	Human-readable name
77-80	ConnectorInfo::x/y	Position for multi-monitor
91	DrmCapture::current_display	Current display ID
screen/dxgi.rs:

Line	Field/Function	Purpose
59	DxgiCapture::current_display	Current display ID
92	is_available()	Check DXGI availability
screen/portal.rs:

Line	Field/Function	Purpose
133	compositor	Detected Wayland compositor
238	get_restore_token()	Get portal restore token
454	was_silent_restore()	Check if last session was silent
460	compositor_capabilities()	Get compositor capabilities
466	compositor()	Get detected compositor
472	recover_session()	Attempt session recovery
492	handle_session_error()	Smart recovery handling
523	unattended_status()	Unattended access status
Related Documentation: docs/design/screen-streaming-phases.md - Phase 7 "Polish & Testing" is IN PROGRESS, explaining why some capabilities exist but aren't fully wired up.

1.8 Manager/Handler Fields
screen/manager.rs:46:


/// Task join handle (kept for future abort/join support)
#[allow(dead_code)]
task_handle: Option<tokio::task::JoinHandle<()>>,
screen/windows_input.rs:39-42:


#[allow(dead_code)]
screen_width: i32,
#[allow(dead_code)]
screen_height: i32,
Purpose: These fields are stored during construction for future coordinate normalization features.

1.9 Daemon State Field
daemon/src/commands/run.rs:13:


pub struct DaemonState {
    #[allow(dead_code)] // Used for future features
    pub config: Config,
    ...
}
2. TODO Comments
2.1 GUI Application TODOs
File: gui/src/app.rs

Line	TODO	Context
1907	Forward to active screen viewer if stream_id matches	Screen frame handling
1913	Notify viewer that stream has ended	Screen stream stop handling
7628	Send quality adjustment request to host	ViewerEvent::QualityAdjustmentNeeded
7632	Send keyframe request to host	ViewerEvent::KeyframeNeeded
7636	Send bitrate adjustment to host	ViewerEvent::BitrateAdjustmentNeeded
Related Documentation: docs/screen-streaming-improvements.md - Section 1.2 "Frame Pacing and Sync Detection" and Section 1.3 "Adaptive Bitrate Algorithm" describe these quality adjustment features.

2.2 Transfer Module TODOs
File: core/src/iroh/transfer.rs

Line	TODO	Context
1520	Add H.264/H.265 hardware encoding for lower latency	Screen streaming encoding
1693	Apply quality/fps changes	ScreenStreamAdjust handling
Related Documentation: docs/ffmpeg-encoding-feature.md - Complete specification for video codec integration.

2.3 Screen Manager TODO
File: core/src/screen/manager.rs:382


// TODO: Make compression configurable via settings or stream request
let mut encoder = create_encoder(ScreenCompression::Raw, ScreenQuality::Balanced);
3. todo!() Macro (Unimplemented Placeholders)
File: tests/iroh_integration.rs

Line	Test	Status
202	test_full_trust_handshake	Requires test-relay feature
213	test_file_push	Requires test-relay feature
224	test_file_pull	Requires test-relay feature
235	test_browse_remote	Requires test-relay feature
All these tests are marked #[ignore = "requires specific network conditions"] and will todo!() panic if run. They are placeholders awaiting the test-relay feature implementation.

Running them: cargo test -p croh-core --features test-relay

4. Documentation TODOs (Design Docs)
File: docs/design/screen-streaming.md

Line	TODO	Feature
1548	Use webp crate	WebP encoding
1552	Use x264 or openh264 crate	H.264 encoding
1556	Use vpx crate	VP9 encoding
1560	Use rav1e crate	AV1 encoding
These are in example code blocks showing future encoder implementations.

5. Relationships Between Dead Code and Documentation
Pattern: "Types First, Implementation Later"
The project follows a phased approach documented in multiple places:

CLAUDE.md - References /misc/croh-v2-plan.md as master planning document
misc/croc-gui-v2-plan.md - Complete migration and feature plan
docs/design/screen-streaming-phases.md - Phase tracker showing Phase 7 "IN PROGRESS"
Key Relationships:
Dead Code	Related Doc	Status
PendingIntroduction struct	Part Two of v2 plan (peer networks)	WIP
wait_for_trust_handshake	Part Two (trust establishment)	Deprecated (refactored)
Croc output parsing (detect_waiting, etc.)	Phase 0.2 of migration plan	Future
Portal recovery/capabilities	Phase 4 Wayland docs	Implemented but unused
Screen stream quality adjustment TODOs	screen-streaming-improvements.md	Phase 1 implemented, Phase 2 pending
Integration test todo!()s	test-relay feature	Awaiting feature
6. Summary by Category
Category	Count	Notes
Test utilities	3	Standard test module pattern
WIP features	1	Peer introduction
Deprecated	1	Replaced by background listener
Future croc features	3	Output parsing for enhanced UI
Platform-specific fields	12+	Stored for future multi-monitor/coordinate work
Screen streaming quality	5 TODOs	Adaptive streaming Phase 2
Unimplemented tests	4	Require test-relay feature
Video codec stubs	4	Design doc examples
7. Recommendations
Keep as-is: Most #[allow(dead_code)] items are intentional scaffolding for phased development
Clean up potential: The deprecated wait_for_trust_handshake could be removed if no longer needed as reference
Feature tracking: The test-relay feature gate is blocking 4 integration tests - this could be prioritized
Screen streaming: Phase 7 completion would wire up the portal capabilities and quality adjustment TODOs