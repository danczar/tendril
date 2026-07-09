<p align="center">
  <img src="crates/tendril-ui/ui/tendril-icon-128.png" width="128" height="128" alt="Tendril">
</p>

<h1 align="center">Tendril</h1>

<p align="center">
  Audio stem separation — vocals, drums, bass, and instrumentals.<br>
  Search YouTube or drop in a local file.
</p>

<p align="center">
  <a href="#install">Install</a> &middot;
  <a href="#how-it-works">How It Works</a> &middot;
  <a href="#building-from-source">Build</a> &middot;
  <a href="#license">License</a>
</p>

---

## What It Does

Tendril is a desktop app that splits audio tracks into individual stems using [Demucs](https://github.com/adefossez/demucs).

- **Search YouTube** and download audio directly, or process local files
- **Separate into 4 stems**: vocals, drums, bass, other
- **Create an instrumental mix** (drums + bass + other), optional
- **Loudness-normalize** every output to a target LUFS (default -14, EBU R128 two-pass)
- **Export** to WAV, FLAC, MP3, or AAC
- **GPU-accelerated** — MPS on Apple Silicon, CUDA on NVIDIA GPUs, automatic detection
- **Self-contained** — manages its own dependencies at runtime

## Install

Download the latest release for your platform from the [GitHub Releases](https://github.com/danczar/tendril/releases) page, or [build from source](#building-from-source).

On first launch, Tendril will automatically download:
- **ffmpeg** — audio format conversion
- **yt-dlp** — YouTube audio downloads

Click the download icon in the app header to install the heavier dependencies:
- **Python** (standalone, won't touch your system install)
- **PyTorch** (with CUDA support if an NVIDIA GPU is detected)
- **Demucs** — stem separation model

Everything is stored in your platform's standard data directory — nothing is installed globally.

## How It Works

1. **Search** for a song or paste a YouTube URL
2. **Click +** to add it to the processing queue
3. Tendril downloads the audio, runs it through Demucs, converts each stem to your chosen format, loudness-normalizes every output to your target LUFS, and optionally builds an instrumental mix
4. Output lands in `~/Music/Tendril/` (configurable in settings)
5. Completed jobs stay in the queue for the session — click a Done item to open its output folder

Each output folder contains:
```
Song Name (video_id)/
├── vocals.flac
├── drums.flac
├── bass.flac
├── other.flac
├── instrumental.flac    # if "Create instrumental" is on (default)
└── full_mix.flac        # if "Preserve full mix" is on
```

If a song has already been processed, the result shows a folder icon instead of + to open the output directly.

## Settings

| Setting | Options | Default |
|---|---|---|
| Output format | WAV, FLAC, MP3, AAC | FLAC |
| GPU backend | Auto, CPU | Auto |
| Model | Demucs (fast), Demucs Fine-tuned (slower, better quality) | Fine-tuned |
| Preserve full mix | Saves the original audio alongside stems | Off |
| Create instrumental | Renders drums + bass + other as `instrumental.{ext}` | On |
| Target loudness | EBU R128 LUFS target applied to every output, -30 to -6 | -14 LUFS |
| Output directory | Any folder | `~/Music/Tendril` |

**Auto** uses MPS on Apple Silicon, CUDA on NVIDIA GPUs, and CPU otherwise.

## Building from Source

### Requirements

- **Rust** 1.88+ ([rustup.rs](https://rustup.rs)) — edition 2024
- **Platform SDK**: Xcode Command Line Tools on macOS, Visual Studio Build Tools on Windows

### Build

```sh
git clone https://github.com/danczar/tendril.git
cd tendril
cargo build --release
```

The binary is at `target/release/Tendril`.

### Project Structure

```
tendril/
├── crates/
│   ├── tendril-core/    # Business logic — no UI dependency
│   │   └── src/
│   │       ├── audio/       # ffmpeg conversion + mixing
│   │       ├── deps/        # Runtime dependency management
│   │       ├── pipeline/    # Job queue + orchestrator
│   │       ├── splitter/    # Demucs subprocess runner
│   │       └── youtube/     # Search + download
│   └── tendril-ui/      # Desktop app (Slint)
│       ├── src/
│       │   ├── main.rs      # Entry point
│       │   ├── bridge.rs    # Rust ↔ Slint wiring
│       │   ├── models.rs    # Data adapters
│       │   └── state.rs     # Shared state
│       └── ui/
│           ├── main-window.slint
│           └── widgets/     # Theme, components
```

`tendril-core` has no UI dependency and can be used independently.

## Platforms

| Platform | GPU | Status |
|---|---|---|
| macOS (Apple Silicon) | MPS | Supported |
| macOS (Intel) | CPU | Supported |
| Windows (x64) | CUDA / CPU | Supported |
| Linux (x64) | CUDA / CPU | Build from source (CI-checked, no packaged release) |

## License

[GPL-3.0-or-later](LICENSE)
