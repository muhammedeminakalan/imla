//! Dot scope: the recording indicator as a field of dots lit by a braided
//! pair of oscilloscope traces that follow the voice. [`Tuning`] holds the
//! defaults; [`Scope`] steps the simulation at a fixed
//! 240 Hz and yields one brightness pair per dot for the shader.

mod scope;
pub use scope::*;

pub const MAX_COLUMNS: usize = 96;
pub const MAX_ROWS: usize = 15;
pub const MAX_DOTS: usize = 1024;
/// The plate height the HUD window is laid out around.
pub const PLATE_HEIGHT: f32 = 26.0;

/// Dot scope's live parameters. Every default is the prototype's value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tuning {
    pub columns: f32,
    pub rows: f32,
    /// Dot pitch, pt.
    pub spacing: f32,
    /// Plate width while recording, pt.
    pub width: f32,
    /// Plate width while transcribing and processing, pt.
    pub processing_width: f32,
    pub height: f32,
    /// Multiplies how fast the braid's components and carrier travel.
    pub braid_speed: f32,
    /// Helix turns across the plate.
    pub helix_cycles: f32,
    /// Multiplies the microphone level.
    pub gain: f32,
    /// The amplitude spring's frequency, Hz.
    pub response: f32,
    /// How long the level takes to fall, s.
    pub release: f32,
    /// How fast the voice's history travels out from the centre, pt/s.
    pub history_speed: f32,
    /// The phosphor tail, s.
    pub phosphor: f32,
    /// Multiplies the glow behind the voice.
    pub glow: f32,
    /// Multiplies the colours' saturation.
    pub saturation: f32,
    /// One processing beam pass, s.
    pub shimmer_pass: f32,
    /// Multiplies the processing beam's brightness.
    pub shimmer: f32,
    /// Enter, exit, and morph duration, s.
    pub transition: f32,
    pub bounce: f32,
    /// Follows the speaker's recent peak instead of the prototype's fixed
    /// −40 to −10 dB window ([`AutoLevel`]).
    pub auto_level: bool,
    /// How long the auto level takes to forget a loud peak, s.
    pub auto_release: f32,
    /// Input below this never lights the scope, and the auto level never
    /// boosts the room up to full, dBFS.
    pub noise_floor: f32,
    /// How transcribing and processing look.
    pub processing: Processing,
}

/// How Dot scope says "working" while a dictation transcribes and
/// processes. Every look settles into done the same way: the field clears
/// from the middle out as it collapses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Processing {
    /// Round 1: the braid irons flat and a beam sweeps back and forth.
    Sweep,
    /// The braid keeps turning and light flows through it left to right.
    #[default]
    Flow,
    /// Pulses leave the middle the way the voice did.
    Pulse,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            columns: 43.0,
            rows: 9.0,
            spacing: 2.6,
            width: 112.0,
            processing_width: 76.0,
            height: PLATE_HEIGHT,
            braid_speed: 1.0,
            helix_cycles: 1.6,
            gain: 1.0,
            response: 6.5,
            release: 0.15,
            history_speed: 150.0,
            phosphor: 0.6,
            glow: 1.0,
            saturation: 1.0,
            shimmer_pass: 0.55,
            shimmer: 1.0,
            transition: 0.3,
            bounce: 0.15,
            auto_level: true,
            auto_release: 3.0,
            noise_floor: -60.0,
            processing: Processing::default(),
        }
    }
}

impl Tuning {
    /// A wide field gets fewer rows so it stays within [`MAX_DOTS`].
    pub fn clamped(mut self) -> Self {
        self.columns = self.columns.clamp(11.0, MAX_COLUMNS as f32).round();
        let rows = (MAX_DOTS / self.columns as usize).min(MAX_ROWS);
        self.rows = self.rows.clamp(3.0, rows as f32).round();
        self
    }
}

/// A meter's RMS as dBFS.
pub fn loudness(rms: f32) -> f32 {
    20.0 * (rms.max(0.0) + 1e-6).log10()
}

/// The prototype's level: −40 dB silent through −10 dB full. A laptop
/// microphone at talking distance peaks near −30 dB, so speech rarely
/// lit a third of it.
pub fn fixed_level(loudness: f32) -> f32 {
    ((loudness + 40.0) / 30.0).clamp(0.0, 1.0)
}

/// The quietest the speaker's peak may be thought to be, above the room.
const AUTO_MIN_SPAN: f32 = 16.0;
/// How far below the peak the level starts.
const AUTO_RANGE: f32 = 24.0;
/// How far above the room the level starts.
const AUTO_ROOM_MARGIN: f32 = 5.0;
const AUTO_ATTACK: f32 = 0.04;
const ROOM_FALL: f32 = 0.3;
const ROOM_RISE: f32 = 6.0;

/// A slow auto level: the loudest the speaker has recently been reads as
/// full and the room as silent, so a quiet microphone and a loud one move
/// the scope alike. The peak rises within 40 ms and falls back over
/// `release`; the room level falls to any quieter moment in 0.3 s and
/// creeps up over 6 s, so speech never becomes the room. The peak never
/// sits less than 16 dB above the room or `noise_floor`, so silence is
/// never boosted to full.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AutoLevel {
    peak: f32,
    room: f32,
}

impl Default for AutoLevel {
    /// A laptop microphone at talking distance: peaks near −30 dB over a
    /// room near −55 dB.
    fn default() -> Self {
        Self {
            peak: -30.0,
            room: -55.0,
        }
    }
}

impl AutoLevel {
    /// Steps `h` seconds at `loudness` dBFS and returns the 0–1 level.
    pub fn step(&mut self, loudness: f32, h: f32, release: f32, noise_floor: f32) -> f32 {
        let db = loudness.max(-120.0);
        let follow = |value: &mut f32, target: f32, time: f32| {
            *value += (target - *value) * (1.0 - (-h / time.max(1e-3)).exp());
        };
        let room_time = if db < self.room { ROOM_FALL } else { ROOM_RISE };
        follow(&mut self.room, db, room_time);
        let lowest = self.room.max(noise_floor) + AUTO_MIN_SPAN;
        if db > self.peak {
            follow(&mut self.peak, db, AUTO_ATTACK);
        } else {
            follow(&mut self.peak, lowest, release);
        }
        self.peak = self.peak.max(lowest);
        self.level(db, noise_floor)
    }

    /// Where `loudness` dBFS sits in the current window, 0 to 1, without
    /// learning from it.
    pub fn level(&self, loudness: f32, noise_floor: f32) -> f32 {
        let low = (self.room.max(noise_floor) + AUTO_ROOM_MARGIN).max(self.peak - AUTO_RANGE);
        ((loudness - low) / (self.peak - low).max(1.0)).clamp(0.0, 1.0)
    }

    /// The speaker's recent peak and the room, dBFS.
    #[cfg(test)]
    pub fn levels(&self) -> (f32, f32) {
        (self.peak, self.room)
    }
}
