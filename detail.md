# Technical detail

## Why the usual approaches do not work

On macOS, essentially every "read NetEase Cloud Music's local files" approach is dead:

- **`file_storage/webdata/file/history`** — the client stopped writing it. The file-writing mechanism itself is still alive (you can catch `{"method":"call","command":"storage.savetofile","params":[...,"webdata\\file\\crash_report",...]}` on the native bridge); only the `history` writer is gone. An OBS script built on it keeps showing whatever was playing a month ago, without erroring and while looking perfectly healthy — the worst way for something to break.
- **The `historyTracks` SQLite table** — schema intact, columns all there, but the client batches writes (it appears to flush on exit), so the current track is never in it.
- **MediaRemote (`nowplaying-cli`)** — only pushes metadata on track change. With `playbackRate=1` (actively playing), `kMRMediaRemoteNowPlayingInfoElapsedTime` measures a constant **0**, and `get-raw` does not even include the `kMRMediaRemoteNowPlayingInfoTimestamp` anchor field. Extrapolating from a wall clock only works if you happen to catch the moment the track changes; start mid-song, or let the user drag the scrubber, and you are permanently off with no way to recover. It is usable as a source for title, artist, album, duration and artwork — nothing more.
- **Injection (the `hooker` approach netease-watcher uses on Windows)** — a non-starter on macOS. The main binary runs under the hardened runtime with only `com.apple.security.cs.allow-jit` and `com.apple.security.cs.disable-library-validation` entitlements. Without `com.apple.security.cs.allow-dyld-environment-variables`, `DYLD_INSERT_LIBRARIES` is refused by the system.

## What actually happens

The macOS player is native — there is no `<audio>` or `<video>` element anywhere in the page DOM. Native code pushes the position to the CEF renderer **once per second** over the synchronous `window.ncmChannelOSX` bridge. The raw call:

```json
{"method":"enData","params":["{\"current\":106,\"resourceDuration\":561,\"resourceId\":\"33638483\",\"trackId\":\"33638483\",\"cacheProgress\":\"1.0000\",\"quality\":320}"]}
```

`current` increments by one every second. The **return value** of that call is the ciphertext, which is then written to localStorage under `lastPlaying`:

```
~/Library/Application Support/com.netease.163music/Documents/storage/CEFCache/Local Storage/leveldb/
```

### Why the ciphertext looks like ECB

Across consecutive writes, the first 22 base64 characters change while the remaining 200-odd are byte-for-byte identical. CBC cannot behave that way — this is ECB, where each 16-byte block is independent, so only the first dozen or so bytes of plaintext are changing.

The reason is the frontend's `initFormatter` template, which pins the key order:

```js
initFormatter(e => Se({
  current: 0, resourceDuration: 0, resourceId: "",
  trackId: "", cacheProgress: 0, quality: B.b.exhigh
}, e))
```

`current` comes first, so it lands in the first AES block. Every other field is constant within a track, leaving the later blocks unchanged.

### Encryption: no need to break it

The frontend's `EncryptData.encode/decode` just forwards to native:

```js
encode(e){ const t={method:"enData",params:[e]};
  return window.ncmChannelOSX.callSyncWithParams(JSON.stringify(t))||"" }
decode(e){ const t={method:"deData",params:[e]}; ... }
```

**The key lives in the native binary, not in the JS bundle.** The one AES key present in the bundle, `"aaaaaaaaaaaaaaaa"` (AES-128-ECB / PKCS7), is only the fallback branch inside `decode()` for when the native call returns empty; running it against a real sample with openssl gives `bad decrypt`, so it is not the production key.

But no key is needed — `deData` sits on the same bridge, and a round trip checks out:

```
{"hello":"world","n":42}  →  BimE0qjzxyYhe3ZrRVeCjb1Og7c1tpBGx5ZFbv9g87k=  →  {"hello":"world","n":42}
```

So this service attaches to CEF DevTools, reads `lastPlaying`, and calls `deData` to decrypt it.

### Lyrics

The client itself uses `/api/song/lyric/v1`, which returns `lrc`, `yrc`, `klyric`, `tlyric` and `romalrc`. Its local cache goes through `Storage.getLyricFromCache(id)` on the native bridge, and the backing store is not visible from JS — not worth chasing, because:

**a plain `fetch` from the page context works.** Same origin, session cookies already attached, no eapi signing to deal with. Song detail comes from `/api/song/detail` the same way.

A yrc line is `[start,duration](t,dur,0)word(t,dur,0)word...`, with the first few lines being bare-JSON metadata (`{"t":0,"c":[{"tx":"作词: "}...]}`). The parser tells them apart by whether the line starts with `[`.

## API

### `GET /`

```json
{
  "playing": true,
  "song": {
    "id": "33638484",
    "name": "Symphony No.9 in E minor, Op.95 \"From the New World\": II.Largo",
    "artists": ["Herbert von Karajan", "Berliner Philharmoniker"],
    "album": "Karajan - Complete Recordings on Deutsche Grammophon",
    "albumPic": "https://p3.music.126.net/...jpg",
    "durationMs": 790894
  },
  "lyrics": {
    "kind": "yrc",
    "lines": [
      {
        "time": 15600,
        "duration": 3970,
        "text": "不小心回到那一天",
        "words": [{ "time": 15600, "duration": 290, "text": "不" }],
        "trans": null
      }
    ]
  },
  "currentSec": 523,
  "positionMs": 523759,
  "durationSec": 790,
  "lineIndex": 0,
  "quality": 320,
  "cacheProgress": "1.0000"
}
```

- `lyrics.kind` — `yrc` (word-level), `lrc` (line-level), or `none` (instrumental or unavailable)
- `words` — populated for `yrc`, an empty array for `lrc`
- `trans` — translation, matched to its lyric line by nearest timestamp within 300ms
- `lineIndex` — index into `lyrics.lines` for the active line

### `GET /ws`

On connect you receive one `snapshot`, then:

| `type` | When | Payload |
| --- | --- | --- |
| `snapshot` | on connect; also to resynchronise a client that fell behind | `data` is the full snapshot |
| `musicchange` | track changed | `data` is the full snapshot, including the new lyrics |
| `timechange` | the second ticked (1 Hz) | `playing` / `currentSec` / `positionMs` / `lineIndex` |
| `idle` | the app quit, or the play record was cleared | none |

```json
{"type":"timechange","playing":true,"currentSec":547,"positionMs":547000,"lineIndex":0}
```

## Position accuracy

The client only reports **whole seconds**. Using them directly for word-level highlighting would advance in visible steps, so:

- the server records the wall-clock instant of each tick and interpolates `positionMs` between them
- `/overlay` keeps interpolating from its own clock after each `timechange`, and drives the word fill from `requestAnimationFrame`

Note that `positionMs` in WebSocket events always equals `currentSec * 1000` — the event fires at the instant of the tick, so there is nothing to interpolate yet. `GET /` returns the interpolated value (`523759`, say). This is deliberate, not a bug: for smooth output, a client should extrapolate from its own clock starting at the moment the event arrives.

**Pause detection is inferred.** The client does not report a play/pause state separately, so the only signal is "no tick for more than 2.5 seconds". A pause therefore takes up to 2.5 seconds to show up.

## Cache

Song detail plus lyrics are stored per track id as a single JSON file:

```
~/Library/Caches/ncm-nowplaying/lyrics/<trackId>.json
```

On a track change the cache is consulted first; a hit means no network request at all. Entries never expire, so if lyrics were corrected upstream, delete the file:

```bash
rm ~/Library/Caches/ncm-nowplaying/lyrics/<trackId>.json
```

The track id comes from the client, so it is filtered through a character allowlist before being used as a filename.

## Known limitations

- Depends on `--remote-debugging-port`, which exposes the renderer wholesale to localhost. Both 9222 and 3574 bind to the loopback interface only; keep it that way.
- If a NetEase update changes the shape of `lastPlaying` or renames the `deData` bridge method, this breaks. Only two places need updating: `RawState` in `src/model.rs`, and the two injected JS snippets in `src/cdp.rs`.
- For instrumentals (`pureMusic: true`) the API returns a single placeholder line. That is the API's own behaviour.
- `romalrc` (romanised lyrics) is fetched but unused; add a field to `TrackPayload` if you need it.

## References

- [YUCLing/netease-watcher](https://github.com/YUCLing/netease-watcher) — the Windows approach (memory, database, injection). The `musicchange` / `timechange` event names here follow its convention.
