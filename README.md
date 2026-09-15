# NexusForge

NexusForge is a Windows-based device collaboration tool that enables seamless
file transfer, clipboard sync, and input sharing between nearby devices.

## Features

- **File transfer** — reliable file sync over TCP
- **Clipboard sync** — share clipboard content across devices
- **Device discovery** — automatic nearby device discovery via UDP heartbeat
- **Local persistence** — lightweight local storage with SQLite
- **Native Windows integration** — uses system-native APIs (e.g. Windows.Media.Ocr) where available

## Tech Stack

- Python
- Windows system APIs
- TCP (data transfer) / UDP (device discovery)
- SQLite (local data persistence)

## Getting Started

### Prerequisites

- Windows 10 or later
- Python 3.10+

### Setup

```bash
# Clone the repository
git clone <repository-url>
cd NexusForge

# Create and activate a virtual environment
python -m venv .venv
.venv\Scripts\activate

# Install dependencies
pip install -r requirements.txt
```

### Running

```bash
python -m nexusforge
```

## Project Status

Under active development — P0 core features first.
