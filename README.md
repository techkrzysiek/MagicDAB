# <img src="web/public/favicon.svg" width="36" alt=""> MagicDAB

A DAB/DAB+ receiver with a web interface. One receiver serves many
listeners in the browser, each on the station of their choice.

- **Inputs:** RTL-SDR, rtl_tcp, ETI/EDI streams, raw IQ recordings.
- **Decoding:** written from scratch in Rust: OFDM, FIC/MSC, DAB+,
  slideshow, DLS/DL+, SPI/EPG, TII.
- **DX:** band scan and DX scan, TII with a transmitter database and map, a
  change log of the multiplexes (also to Discord), ETI recordings of
  multiplexes heard for the first time.
- **Extras:** RadioDNS and RadioVIS, DAB/FM service following with an
  FM-DX-Tuner (TEF668x), loudness meter (EBU R128), public tunnel.
- **Packages:** Docker, Linux, Windows application with updates, macOS
  (untested).

![Listening: the station with its slideshow and text, the stations of the multiplex and of all multiplexes](docs/screenshots/listen.png)

| Ensemble | Spectrum |
|---|---|
| ![Ensemble: the multiplex, its capacity units and services](docs/screenshots/ensemble.png) | ![Spectrum, SNR, constellation, impulse response](docs/screenshots/spectrum.png) |


## Supported tuners and inputs

| Input | Notes |
|---|---|
| RTL-SDR USB sticks (RTL2832U with R820T / R828D) | e.g. RTL-SDR Blog V3 and V4, with bias-T; the packages bring librtlsdr (rtl-sdr-blog) |
| rtl_tcp | an RTL-SDR on another computer, over the network |
| DTR-100 / wRadio C100 USB DAB+ dongle | the dongle demodulates by itself: no spectrum, TII, DX scan or gain control |
| ETI / EDI streams | `eti+tcp://host:port`, `edi+tcp://host:port`, mapped to channels |
| IQ recordings | `.uff`, raw u8 / s16 at 2.048 MS/s, played as channels |
| FM-DX-Tuner (TEF668x) | FM only, for DAB/FM service following |

## Running

- Docker: `docker compose up -d` (see `docker-compose.yml`), web interface on
  port 8080; the admin password is logged at the first start. The image is
  `ghcr.io/techkrzysiek/magicdab`; Watchtower in the same file installs each new
  release at night.
- Windows: `build/windows.sh` builds `dist/windows/MagicDAB-<version>-windows-x64.zip`
  (see `build/windows/README.txt`).
- Linux (x64, arm64): `build/linux.sh` builds `dist/linux/MagicDAB-<version>-linux-<arch>.zip`
  for the architecture of the build host: one program with the interface
  inside and the RTL-SDR library next to it (see `build/linux/README.txt`).
- macOS (untested): `build/macos.sh` on a Mac builds one program for Apple
  silicon and Intel (see `build/macos/README.txt`).
- From source: `cargo build --release -p magicdab` (needs libfaad, libopus,
  librtlsdr) and `npm ci && npm run build` in `web/`.

## Releases

The Release workflow (each push to main that changes the program, a pushed
`v*` tag, or the Actions tab) builds the packages for Windows, Linux (x64,
and arm64 in a public repository) and macOS and the Docker image
(`ghcr.io/techkrzysiek/magicdab`, the server alone) and publishes them with
the release notes. Installed copies find new versions in the update
manifest of the latest release and install them from Settings > Internet >
Updates.

## License

Copyright (C) 2026 techkrzysiek

MagicDAB is free software under the GNU Affero General Public License v3 or
later (`LICENSE`), with an additional permission for the Microsoft WebView2
and Visual C++ runtime libraries of the Windows package
(`LICENSE-EXCEPTION.txt`). Whoever offers a modified MagicDAB to others over a
network must offer them its source code too (AGPL section 13): set its
address in Settings > Webserver > Source code address, it is shown in About.

The name MagicDAB and its logo are not covered by the license: a modified
version offered to others needs another name.

Third-party components and their licenses: `THIRD-PARTY.md`.
