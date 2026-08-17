# ncm-nowplaying

把网易云音乐(v3.x) **macOS 版**的实时播放进度和逐字歌词，通过 HTTP / WebSocket 暴露出来。自带歌词磁盘缓存，和一个 OBS 透明歌词 Browser Source。

## 安装

```bash
brew tap wesleyel/tap
brew install wesleyel/tap/ncm-nowplaying
```

或者自己编：

```bash
cargo build --release
```

## 前提

网易云必须带调试端口启动，正常双击图标启动的没有这个端口：

```bash
osascript -e 'quit app "NeteaseMusic"' && open -a NeteaseMusic --args --remote-debugging-port=9222
```

## 运行

```bash
ncm-nowplaying
```

```
[serve] http://127.0.0.1:3574/  ws://127.0.0.1:3574/ws  overlay http://127.0.0.1:3574/overlay
[cache] /Users/you/Library/Caches/ncm-nowplaying/lyrics
[cdp] 已连上渲染进程
[track] 108965 冻结 歌词=Yrc 83 行
```

网易云没开、或者中途退出，程序会一直重连，不用管。

用 Homebrew 装的话也可以挂后台：

```bash
brew services start ncm-nowplaying
```

## 配置

| 环境变量 | 默认 | 说明 |
| --- | --- | --- |
| `HOST` | `127.0.0.1` | 监听地址 |
| `PORT` | `3574` | 监听端口 |
| `NCM_DEVTOOLS_PORT` | `9222` | 网易云的调试端口 |
| `NCM_POLL_MS` | `250` | 轮询间隔 |
| `NCM_CACHE_DIR` | `~/Library/Caches/ncm-nowplaying/lyrics` | 歌词缓存目录 |

## 接口

| 路径 | 说明 |
| --- | --- |
| `GET /` | 当前快照：歌曲信息、完整歌词、进度、音质 |
| `GET /ws` | 事件推送。连上先收一条 `snapshot`，之后是 `musicchange`（换歌）/ `timechange`（1 Hz）/ `idle` |
| `GET /overlay` | 透明背景歌词页 |

字段含义和完整 JSON 见 [detail.md](detail.md#api)。

## OBS

加一个「浏览器」源，URL 填 `http://127.0.0.1:3574/overlay`，宽高按画面来。页面本身就是透明背景，不需要额外的自定义 CSS。

Overlay 面板最大尺寸为 **860 × 206 CSS 像素**（宽 × 高），且没有外部留白。OBS 浏览器源可直接设为 `860 × 206`；使用更小的画布时，面板和字号会自动收缩。

逐字歌词会随播放进度填充；没有逐字歌词的歌退化成整行渐进；纯音乐只显示「纯音乐，请欣赏」。

## 许可

MIT
