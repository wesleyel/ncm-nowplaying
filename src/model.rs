use serde::{Deserialize, Serialize};

/// The plaintext behind `lastPlaying`. Field order matches the client's
/// `initFormatter` template.
#[derive(Debug, Clone, Deserialize)]
pub struct RawState {
    pub current: f64,
    #[serde(rename = "resourceDuration")]
    pub resource_duration: f64,
    #[serde(rename = "trackId")]
    pub track_id: serde_json::Value,
    #[serde(rename = "cacheProgress", default)]
    pub cache_progress: serde_json::Value,
    #[serde(default)]
    pub quality: serde_json::Value,
}

impl RawState {
    pub fn track_id(&self) -> String {
        match &self.track_id {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Song {
    pub id: String,
    pub name: String,
    pub artists: Vec<String>,
    pub album: String,
    #[serde(rename = "albumPic")]
    pub album_pic: String,
    #[serde(rename = "durationMs")]
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Word {
    pub time: u64,
    pub duration: u64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Line {
    pub time: u64,
    pub duration: u64,
    pub text: String,
    /// Per-word timings; empty when the source was plain `lrc`.
    pub words: Vec<Word>,
    /// Translation for this line, if any.
    #[serde(rename = "trans")]
    pub translation: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LyricKind {
    /// Word-level lyrics
    Yrc,
    /// Line-level lyrics
    Lrc,
    /// Instrumental, or no lyrics available
    None,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Lyrics {
    pub kind: LyricKind,
    pub lines: Vec<Line>,
}

/// The full state this service exposes.
#[derive(Debug, Clone, Serialize, Default)]
pub struct Snapshot {
    /// The app is running and the position is advancing
    pub playing: bool,
    pub song: Option<Song>,
    pub lyrics: Option<Lyrics>,
    /// Whole-second position as reported by the client
    #[serde(rename = "currentSec")]
    pub current_sec: u64,
    /// Position interpolated to milliseconds, for word-level highlighting
    #[serde(rename = "positionMs")]
    pub position_ms: u64,
    #[serde(rename = "durationSec")]
    pub duration_sec: u64,
    /// Index of the active lyric line
    #[serde(rename = "lineIndex")]
    pub line_index: Option<usize>,
    /// Audio quality tier as reported by the client (e.g. 320)
    pub quality: serde_json::Value,
    /// Buffering progress as reported by the client
    #[serde(rename = "cacheProgress")]
    pub cache_progress: serde_json::Value,
}
