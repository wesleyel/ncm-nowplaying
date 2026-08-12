//! Lyric parsing and on-disk caching.
//!
//! A yrc (word-level) line looks like:
//! `[368,1820](368,290,0)海(658,271,0)风 ...`
//! The first few lines are pure-JSON metadata (`{"t":0,"c":[{"tx":"作词: "}...]}`)
//! and are skipped.
//!
//! `lrc` and `tlyric` use the conventional `[mm:ss.xx]text` form.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::model::{Line, LyricKind, Lyrics, Song, Word};

#[derive(Debug, Clone, Deserialize)]
pub struct TrackPayload {
    pub song: Option<Song>,
    pub yrc: Option<String>,
    pub lrc: Option<String>,
    pub tlyric: Option<String>,
}

impl TrackPayload {
    pub fn into_parts(self) -> (Option<Song>, Lyrics) {
        let trans = self.tlyric.as_deref().map(parse_lrc).unwrap_or_default();

        let (kind, mut lines) = match self.yrc.as_deref().map(parse_yrc) {
            Some(l) if !l.is_empty() => (LyricKind::Yrc, l),
            _ => match self.lrc.as_deref().map(parse_lrc) {
                Some(l) if !l.is_empty() => (LyricKind::Lrc, l),
                _ => (LyricKind::None, Vec::new()),
            },
        };
        attach_translation(&mut lines, &trans);

        (self.song, Lyrics { kind, lines })
    }
}

/// Match translations to their lyric line by nearest timestamp, within 300ms.
fn attach_translation(lines: &mut [Line], trans: &[Line]) {
    for line in lines.iter_mut() {
        if let Some(t) = trans
            .iter()
            .find(|t| t.time.abs_diff(line.time) <= 300 && !t.text.is_empty())
        {
            line.translation = Some(t.text.clone());
        }
    }
}

pub fn parse_yrc(text: &str) -> Vec<Line> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let raw = raw.trim();
        // Metadata lines are bare JSON, without the `[start,dur]` prefix.
        if !raw.starts_with('[') {
            continue;
        }
        let Some((head, body)) = raw[1..].split_once(']') else {
            continue;
        };
        let mut head = head.split(',');
        let (Some(time), Some(duration)) = (
            head.next().and_then(|s| s.trim().parse::<u64>().ok()),
            head.next().and_then(|s| s.trim().parse::<u64>().ok()),
        ) else {
            continue;
        };

        let words = parse_words(body);
        let text: String = words.iter().map(|w| w.text.as_str()).collect();
        if text.trim().is_empty() {
            continue;
        }
        out.push(Line {
            time,
            duration,
            text: text.trim().to_owned(),
            words,
            translation: None,
        });
    }
    out.sort_by_key(|l| l.time);
    out
}

/// `(t,dur,0)text(t,dur,0)text...`
fn parse_words(body: &str) -> Vec<Word> {
    let mut words = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != b'(' {
            // Stray text outside any group; should not happen, skip it so we cannot loop forever.
            i += 1;
            continue;
        }
        let Some(close) = body[i..].find(')').map(|p| i + p) else {
            break;
        };
        let mut meta = body[i + 1..close].split(',');
        let time = meta.next().and_then(|s| s.trim().parse::<u64>().ok());
        let duration = meta.next().and_then(|s| s.trim().parse::<u64>().ok());

        let text_start = close + 1;
        let text_end = body[text_start..]
            .find('(')
            .map(|p| text_start + p)
            .unwrap_or(body.len());

        if let (Some(time), Some(duration)) = (time, duration) {
            let text = &body[text_start..text_end];
            if !text.is_empty() {
                words.push(Word {
                    time,
                    duration,
                    text: text.to_owned(),
                });
            }
        }
        i = text_end;
    }
    words
}

pub fn parse_lrc(text: &str) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();

    for raw in text.lines() {
        let raw = raw.trim();
        // Skip yrc metadata lines that may have leaked in.
        if raw.starts_with("{\"") {
            continue;
        }
        let mut stamps = Vec::new();
        let mut rest = raw;
        while rest.starts_with('[') {
            let Some((head, tail)) = rest[1..].split_once(']') else {
                break;
            };
            match parse_stamp(head) {
                Some(ms) => stamps.push(ms),
                // A tag such as `[ti:xxx]`; drop the whole line.
                None => {
                    stamps.clear();
                    break;
                }
            }
            rest = tail;
        }
        let body = rest.trim();
        if stamps.is_empty() || body.is_empty() {
            continue;
        }
        for time in stamps {
            out.push(Line {
                time,
                duration: 0,
                text: body.to_owned(),
                words: Vec::new(),
                translation: None,
            });
        }
    }

    out.sort_by_key(|l| l.time);
    // lrc carries no line duration; derive it from the start of the next line.
    for i in 0..out.len() {
        let end = out.get(i + 1).map(|n| n.time).unwrap_or(out[i].time + 5_000);
        out[i].duration = end.saturating_sub(out[i].time);
    }
    out
}

/// `mm:ss`, `mm:ss.xx` or `mm:ss.xxx` -> milliseconds
fn parse_stamp(head: &str) -> Option<u64> {
    let (min, rest) = head.split_once(':')?;
    let min: u64 = min.trim().parse().ok()?;
    let (sec, frac) = match rest.split_once(['.', ':']) {
        Some((s, f)) => (s, f),
        None => (rest, ""),
    };
    let sec: u64 = sec.trim().parse().ok()?;
    let frac_ms = match frac.len() {
        0 => 0,
        1 => frac.parse::<u64>().ok()? * 100,
        2 => frac.parse::<u64>().ok()? * 10,
        _ => frac[..3].parse::<u64>().ok()?,
    };
    Some((min * 60 + sec) * 1000 + frac_ms)
}

/// Binary search for the line covering `position_ms`.
pub fn line_at(lines: &[Line], position_ms: u64) -> Option<usize> {
    if lines.is_empty() || position_ms < lines[0].time {
        return None;
    }
    let idx = lines.partition_point(|l| l.time <= position_ms);
    Some(idx.saturating_sub(1))
}

// ---------------------------------------------------------------- disk cache

pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    pub fn new(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("cannot create cache directory {}", dir.display()))?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, id: &str) -> PathBuf {
        // The track id comes from the client; allowlist its characters so it cannot
        // escape the cache directory.
        let safe: String = id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        self.dir.join(format!("{safe}.json"))
    }

    pub fn get(&self, id: &str) -> Option<TrackPayload> {
        let text = std::fs::read_to_string(self.path(id)).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn put(&self, id: &str, payload: &TrackPayload) {
        let path = self.path(id);
        let body = match serde_json::to_string(&serde_json::json!({
            "song": payload.song,
            "yrc": payload.yrc,
            "lrc": payload.lrc,
            "tlyric": payload.tlyric,
        })) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[cache] cannot serialize {id}: {e}");
                return;
            }
        };
        if let Err(e) = std::fs::write(&path, body) {
            eprintln!("[cache] cannot write {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yrc_line_with_words() {
        let lines = parse_yrc("{\"t\":0,\"c\":[{\"tx\":\"作词: \"}]}\n[368,1820](368,290,0)海(658,271,0)风");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].time, 368);
        assert_eq!(lines[0].duration, 1820);
        assert_eq!(lines[0].text, "海风");
        assert_eq!(lines[0].words.len(), 2);
        assert_eq!(lines[0].words[1].time, 658);
    }

    /// Real yrc taken from the live API (trackId 108965).
    #[test]
    fn yrc_from_production() {
        let raw = "[15600,3970](15600,290,0)不(15890,550,0)小(16440,510,0)心(16950,370,0)回(17320,500,0)到(17820,480,0)那(18300,430,0)一(18730,840,0)天\n\
                   [20000,3100](20000,330,0)不(20330,540,0)小(20870,410,0)心(21280,390,0)一(21670,490,0)切(22160,270,0)又(22430,340,0)重(22770,330,0)演";
        let lines = parse_yrc(raw);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].time, 15_600);
        assert_eq!(lines[0].duration, 3_970);
        assert_eq!(lines[0].text, "不小心回到那一天");
        assert_eq!(lines[0].words.len(), 8);
        assert_eq!(lines[0].words[7].time, 18_730);
        assert_eq!(lines[0].words[7].duration, 840);
        assert_eq!(lines[1].text, "不小心一切又重演");
        assert_eq!(line_at(&lines, 21_000), Some(1));
    }

    #[test]
    fn lrc_stamps_and_durations() {
        let lines = parse_lrc("[ti:x]\n[00:01.50]line one\n[00:03.00][00:09.25]line two");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].time, 1500);
        assert_eq!(lines[0].duration, 1500);
        assert_eq!(lines[2].time, 9250);
    }

    #[test]
    fn locate_line() {
        let lines = parse_lrc("[00:01.00]a\n[00:05.00]b");
        assert_eq!(line_at(&lines, 0), None);
        assert_eq!(line_at(&lines, 1_000), Some(0));
        assert_eq!(line_at(&lines, 4_999), Some(0));
        assert_eq!(line_at(&lines, 5_000), Some(1));
    }
}
