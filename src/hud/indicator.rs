// Ported from anomalyco/hex (MIT License, Copyright (c) 2026 Christopher Langton).
// See THIRD_PARTY_NOTICES.md. Original: src/dictation/indicator.rs (state machine only).
//! The indicator model: which look Dot scope shows (recording, transcribing,
//! done, failed) and when it fades out, driven by the dictation events.

use std::time::{Duration, Instant};

use super::dot_scope::{self, Frame, Pose, Scope, Stage, Tuning};

/// Transcribing shows at least this long before done wipes it.
const PROCESSING_MIN_DURATION: Duration = Duration::from_millis(200);
const COMPLETED_VISIBLE_DURATION: Duration = Duration::from_millis(100);
const FAILED_VISIBLE_DURATION: Duration = Duration::from_millis(600);
const SILENT: f32 = -120.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Hidden,
    Recording,
    Transcribing,
    Completed,
    Cancelled,
    Failed,
}

impl Phase {
    fn visible_duration(self) -> Option<Duration> {
        match self {
            Self::Completed => Some(COMPLETED_VISIBLE_DURATION),
            Self::Cancelled => Some(Duration::ZERO),
            Self::Failed => Some(FAILED_VISIBLE_DURATION),
            _ => None,
        }
    }

    /// The look this phase shows; `None` keeps the last one as it fades.
    fn stage(self) -> Option<Stage> {
        match self {
            Self::Recording => Some(Stage::Recording),
            Self::Transcribing => Some(Stage::Processing),
            Self::Completed => Some(Stage::Done),
            Self::Failed => Some(Stage::Failed),
            Self::Hidden | Self::Cancelled => None,
        }
    }
}

pub struct Indicator {
    phase: Phase,
    phase_started: Instant,
    working_since: Instant,
    completion_pending: bool,
    scope: Scope,
    stage: Stage,
    /// The microphone's loudness, dBFS.
    loudness: f32,
    /// From an event until a step finds everything settled out of sight.
    active: bool,
}

impl Indicator {
    pub fn new(now: Instant) -> Self {
        Self {
            phase: Phase::Hidden,
            phase_started: now,
            working_since: now,
            completion_pending: false,
            scope: Scope::new(Tuning::default()),
            stage: Stage::Recording,
            loudness: SILENT,
            active: false,
        }
    }

    /// Whether the HUD wants frames.
    pub fn active(&self) -> bool {
        self.active
    }

    pub fn listen(&mut self, now: Instant) {
        self.active = true;
        self.phase = Phase::Recording;
        self.phase_started = now;
        self.completion_pending = false;
        self.loudness = SILENT;
        self.stage = Stage::Recording;
        self.scope.restart(false);
    }

    /// The microphone's RMS, 0..1 linear.
    pub fn level(&mut self, rms: f32) {
        self.loudness = dot_scope::loudness(rms);
    }

    pub fn busy(&mut self, now: Instant) {
        self.active = true;
        if self.phase != Phase::Transcribing {
            self.working_since = now;
        }
        self.phase = Phase::Transcribing;
        self.phase_started = now;
        self.completion_pending = false;
    }

    pub fn fail(&mut self, now: Instant) {
        self.active = true;
        self.phase = Phase::Failed;
        self.phase_started = now;
        self.completion_pending = false;
    }

    /// Done after transcribing, or a plain fade when it was cancelled while
    /// recording.
    pub fn hide(&mut self, now: Instant) {
        match self.phase {
            Phase::Transcribing => self.completion_pending = true,
            Phase::Recording => {
                self.phase = Phase::Cancelled;
                self.phase_started = now;
            }
            _ => {}
        }
    }

    /// Advances `dt` seconds to `now`.
    pub fn step(&mut self, now: Instant, dt: f32) {
        let dt = dt.clamp(1.0 / 240.0, 1.0 / 20.0);
        if self.completion_pending && now.duration_since(self.working_since) >= PROCESSING_MIN_DURATION {
            self.phase = Phase::Completed;
            self.phase_started = now;
            self.completion_pending = false;
        }
        let elapsed = now.duration_since(self.phase_started);
        let terminal_finished = self.phase.visible_duration().is_some_and(|d| elapsed >= d);
        let visible = self.phase != Phase::Hidden && !terminal_finished;
        if let Some(stage) = self.phase.stage() {
            self.stage = stage;
        }
        self.scope.advance(
            dt,
            Frame { stage: self.stage, visible, loudness: self.loudness, reduce_motion: false },
        );
        self.active = visible || !self.scope.settled();
    }

    pub fn pose(&self) -> Pose {
        self.scope.pose()
    }

    pub fn tuning(&self) -> Tuning {
        self.scope.tuning()
    }

    pub fn columns(&self) -> usize {
        self.scope.columns()
    }

    pub fn rows(&self) -> usize {
        self.scope.rows()
    }

    pub fn dots(&self) -> &[[f32; 2]] {
        self.scope.dots()
    }

    #[cfg(test)]
    fn stage(&self) -> Stage {
        self.stage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(model: &mut Indicator, start: Instant, from: f32, seconds: f32) {
        let mut t = from;
        while t < from + seconds {
            t += 1.0 / 60.0;
            model.step(start + Duration::from_secs_f32(t), 1.0 / 60.0);
        }
    }

    #[test]
    fn a_dictation_records_processes_finishes_and_settles() {
        let start = Instant::now();
        let mut model = Indicator::new(start);
        assert!(!model.active());
        model.listen(start);
        run(&mut model, start, 0.0, 0.5);
        assert_eq!(model.stage(), Stage::Recording);
        model.busy(start + Duration::from_secs_f32(0.5));
        run(&mut model, start, 0.5, 0.5);
        assert_eq!(model.stage(), Stage::Processing);
        model.hide(start + Duration::from_secs_f32(1.0));
        run(&mut model, start, 1.0, 0.3);
        assert_eq!(model.stage(), Stage::Done);
        run(&mut model, start, 1.3, 1.5);
        assert!(!model.active(), "settles out of sight");
    }

    #[test]
    fn a_failure_turns_coral_then_fades() {
        let start = Instant::now();
        let mut model = Indicator::new(start);
        model.listen(start);
        run(&mut model, start, 0.0, 0.5);
        model.fail(start + Duration::from_secs_f32(0.5));
        run(&mut model, start, 0.5, 0.4);
        assert!(model.pose().fail > 0.5);
        run(&mut model, start, 0.9, 2.0);
        assert!(!model.active());
    }
}
