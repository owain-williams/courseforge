//! The recording lifecycle as a pure state machine.
//!
//! A `RecordingSession` is the user-facing handle for one Take against one
//! Video slot. It only models *what state the session is in*; the actual
//! byte-pushing to `.partial.mov` lives behind the `Recorder` trait and the
//! orchestrating manager that owns it.
//!
//! Keeping the state machine pure means we can exhaustively unit-test every
//! legal and illegal transition without spinning up SCK or asking the OS
//! for screen-recording permission.
//!
//! ```text
//!   Idle ──start──► Recording ◄─resume─ Paused
//!                      │ │                │
//!                      │ └────pause───────┘
//!                      │
//!                      ▼ stop                  keep       Persisted
//!                  AwaitingDecision ──────────────────►
//!                      │                                 ↘
//!                      └──────discard──────────────────►   Discarded
//! ```
//!
//! `Persisted` and `Discarded` are terminal: a session is single-use, and any
//! subsequent recording is a fresh session with its own id.
//!
//! Per ADR-0002 the session carries the `Vec<CaptureRequest>` for the Take
//! and the `take_id` that links the per-Segment sidecars together. Phase 1
//! exercises this with one or two requests (screen + optional mic into a
//! single `.mov`); Phase 2 grows to N writers.

use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use crate::core::capture::CaptureRequest;
use crate::core::error::{CoreError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionState {
    Idle,
    Recording,
    Paused,
    AwaitingDecision,
    Persisted,
    Discarded,
}

impl SessionState {
    fn label(self) -> &'static str {
        match self {
            SessionState::Idle => "idle",
            SessionState::Recording => "recording",
            SessionState::Paused => "paused",
            SessionState::AwaitingDecision => "awaitingDecision",
            SessionState::Persisted => "persisted",
            SessionState::Discarded => "discarded",
        }
    }
}

/// One Take. Lives in memory while the user is capturing; once in a terminal
/// state it's safe to drop. The `partial_path` is allocated up front by
/// [`crate::core::segments::prepare_segment_path`] so the recorder backend
/// always has a destination to write to. `recorded_at` is captured at Start
/// time so the per-Segment sidecar (written on Keep) carries an honest
/// "when did this Take begin" rather than the Keep-decision timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingSession {
    pub id: String,
    pub video_id: String,
    pub segment_id: String,
    pub partial_path: PathBuf,
    pub take_id: String,
    pub requests: Vec<CaptureRequest>,
    /// ISO-8601 UTC timestamp the Take started.
    pub recorded_at: String,
    pub state: SessionState,
}

impl RecordingSession {
    pub fn new(
        video_id: String,
        segment_id: String,
        partial_path: PathBuf,
        take_id: String,
        requests: Vec<CaptureRequest>,
        recorded_at: String,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            video_id,
            segment_id,
            partial_path,
            take_id,
            requests,
            recorded_at,
            state: SessionState::Idle,
        }
    }

    pub fn start(&mut self) -> Result<()> {
        self.transition("start", &[SessionState::Idle], SessionState::Recording)
    }

    pub fn pause(&mut self) -> Result<()> {
        self.transition("pause", &[SessionState::Recording], SessionState::Paused)
    }

    pub fn resume(&mut self) -> Result<()> {
        self.transition("resume", &[SessionState::Paused], SessionState::Recording)
    }

    pub fn stop(&mut self) -> Result<()> {
        self.transition(
            "stop",
            &[SessionState::Recording, SessionState::Paused],
            SessionState::AwaitingDecision,
        )
    }

    pub fn mark_persisted(&mut self) -> Result<()> {
        self.transition(
            "keep",
            &[SessionState::AwaitingDecision],
            SessionState::Persisted,
        )
    }

    pub fn mark_discarded(&mut self) -> Result<()> {
        self.transition(
            "discard",
            // Discard works from AwaitingDecision (clean stop, user said no)
            // *and* from Recording/Paused (user wants to abandon mid-flight —
            // the manager will tear the recorder down first).
            &[
                SessionState::AwaitingDecision,
                SessionState::Recording,
                SessionState::Paused,
            ],
            SessionState::Discarded,
        )
    }

    pub fn is_active(&self) -> bool {
        matches!(self.state, SessionState::Recording | SessionState::Paused)
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self.state, SessionState::Persisted | SessionState::Discarded)
    }

    fn transition(
        &mut self,
        action: &str,
        allowed_from: &[SessionState],
        to: SessionState,
    ) -> Result<()> {
        if !allowed_from.contains(&self.state) {
            return Err(CoreError::InvalidSessionTransition {
                action: action.to_string(),
                state: self.state.label().to_string(),
            });
        }
        self.state = to;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::capture::{CompositionDefaults, Device, SourceRole};

    fn screen_request() -> CaptureRequest {
        CaptureRequest {
            role: SourceRole::Screen,
            device: Device {
                id: "default".into(),
                label: "Main Display".into(),
            },
            defaults: CompositionDefaults::default(),
        }
    }

    fn session() -> RecordingSession {
        RecordingSession::new(
            "vid-1".into(),
            "seg-1".into(),
            PathBuf::from("/tmp/seg-1.partial.mov"),
            "take-1".into(),
            vec![screen_request()],
            "2026-05-26T12:00:00Z".into(),
        )
    }

    #[test]
    fn new_session_is_idle_and_carries_its_inputs() {
        let s = session();
        assert_eq!(s.state, SessionState::Idle);
        assert_eq!(s.video_id, "vid-1");
        assert_eq!(s.segment_id, "seg-1");
        assert_eq!(s.take_id, "take-1");
        assert_eq!(s.requests, vec![screen_request()]);
        assert!(!s.id.is_empty());
        assert_eq!(s.partial_path, PathBuf::from("/tmp/seg-1.partial.mov"));
    }

    #[test]
    fn start_moves_idle_to_recording() {
        let mut s = session();
        s.start().unwrap();
        assert_eq!(s.state, SessionState::Recording);
    }

    #[test]
    fn start_from_non_idle_is_an_invalid_transition() {
        let mut s = session();
        s.start().unwrap();
        assert!(matches!(
            s.start(),
            Err(CoreError::InvalidSessionTransition { .. })
        ));
    }

    #[test]
    fn pause_only_works_from_recording() {
        let mut s = session();
        assert!(matches!(s.pause(), Err(CoreError::InvalidSessionTransition { .. })));
        s.start().unwrap();
        s.pause().unwrap();
        assert_eq!(s.state, SessionState::Paused);
        assert!(matches!(s.pause(), Err(CoreError::InvalidSessionTransition { .. })));
    }

    #[test]
    fn resume_only_works_from_paused() {
        let mut s = session();
        s.start().unwrap();
        assert!(matches!(s.resume(), Err(CoreError::InvalidSessionTransition { .. })));
        s.pause().unwrap();
        s.resume().unwrap();
        assert_eq!(s.state, SessionState::Recording);
    }

    #[test]
    fn stop_works_from_recording_or_paused_only() {
        let mut s = session();
        // From Idle: nope.
        assert!(matches!(s.stop(), Err(CoreError::InvalidSessionTransition { .. })));
        s.start().unwrap();
        // From Recording: yes.
        s.stop().unwrap();
        assert_eq!(s.state, SessionState::AwaitingDecision);

        // Set up a Paused session for the second arm.
        let mut t = session();
        t.start().unwrap();
        t.pause().unwrap();
        t.stop().unwrap();
        assert_eq!(t.state, SessionState::AwaitingDecision);
    }

    #[test]
    fn keep_promotes_awaiting_decision_to_persisted_terminal() {
        let mut s = session();
        s.start().unwrap();
        s.stop().unwrap();
        s.mark_persisted().unwrap();
        assert_eq!(s.state, SessionState::Persisted);
        assert!(s.is_terminal());
        // No more transitions out of a terminal state.
        assert!(s.mark_discarded().is_err());
        assert!(s.start().is_err());
    }

    #[test]
    fn discard_works_from_awaiting_decision_recording_or_paused() {
        // From AwaitingDecision (the canonical clean-stop-then-no path).
        let mut s = session();
        s.start().unwrap();
        s.stop().unwrap();
        s.mark_discarded().unwrap();
        assert_eq!(s.state, SessionState::Discarded);

        // Mid-recording abandon: also allowed (UI surfaces "Discard" while
        // active, the manager tears the recorder down then transitions).
        let mut t = session();
        t.start().unwrap();
        t.mark_discarded().unwrap();
        assert_eq!(t.state, SessionState::Discarded);

        // Paused abandon.
        let mut u = session();
        u.start().unwrap();
        u.pause().unwrap();
        u.mark_discarded().unwrap();
        assert_eq!(u.state, SessionState::Discarded);
    }

    #[test]
    fn discard_from_idle_or_terminal_is_invalid() {
        let mut s = session();
        assert!(matches!(s.mark_discarded(), Err(CoreError::InvalidSessionTransition { .. })));

        let mut t = session();
        t.start().unwrap();
        t.stop().unwrap();
        t.mark_persisted().unwrap();
        assert!(matches!(t.mark_discarded(), Err(CoreError::InvalidSessionTransition { .. })));
    }

    #[test]
    fn is_active_is_true_for_recording_and_paused_only() {
        let mut s = session();
        assert!(!s.is_active());
        s.start().unwrap();
        assert!(s.is_active());
        s.pause().unwrap();
        assert!(s.is_active());
        s.resume().unwrap();
        s.stop().unwrap();
        assert!(!s.is_active());
    }

    #[test]
    fn invalid_transition_error_names_the_current_state() {
        let s = session();
        let err = s.clone();
        // Trying to pause from Idle should mention "idle" in the error so the
        // command layer can surface a useful message.
        let mut s = err;
        let e = s.pause().unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("pause"), "msg was {msg}");
        assert!(msg.contains("idle"), "msg was {msg}");
    }
}
