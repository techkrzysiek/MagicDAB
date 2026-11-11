# <img src="web/public/favicon.svg" width="36" alt=""> MagicDAB

A DAB/DAB+ receiver with a web interface. One receiver serves many
listeners in the browser, each on the station of their choice.

- **Inputs:** RTL-SDR, rtl_tcp, HackRF One, DTR-100, ETI/EDI streams, raw
  IQ recordings.
- **Decoding:** written from scratch in Rust: OFDM, FIC/MSC, DAB+,
  slideshow, DLS/DL+, SPI/EPG, TII.
- **DX:** band scan and DX scan, TII with a transmitter database and map, a
  change log of the multiplexes (also to Discord), ETI recordings of
  multiplexes heard for the first time and a report of their first minute
  (mean and peak SNR, transmitters by TII with distance and bearing).
- **Extras:** RadioDNS and RadioVIS, DAB/FM service following with an
  FM-DX-Tuner (TEF668x), loudness meter (EBU R128), public tunnel; on the
  computer it runs on the program plays the sound itself (Windows, Linux
  desktop, Android), the browser decodes nothing.
- **Packages:** Docker, Linux, Windows application with updates, Android app
  (an RTL-SDR stick on USB OTG), macOS (untested).

<p>
  <a href="https://github.com/techkrzysiek/MagicDAB/releases/latest"><img alt="Download for Windows" src="https://img.shields.io/badge/Windows-download-0078D6?style=for-the-badge"></a>
  <a href="https://github.com/techkrzysiek/MagicDAB/releases/latest"><img alt="Download for Linux (x64, arm64)" src="https://img.shields.io/badge/Linux_x64,_arm64-download-FCC624?style=for-the-badge&logo=linux&logoColor=black"></a>
  <a href="https://github.com/techkrzysiek/MagicDAB/releases/latest"><img alt="Download for Android" src="https://img.shields.io/badge/Android-download-3DDC84?style=for-the-badge&logo=android&logoColor=white"></a>
  <a href="https://github.com/techkrzysiek/MagicDAB/pkgs/container/magicdab"><img alt="Docker image" src="https://img.shields.io/badge/Docker-ghcr.io-2496ED?style=for-the-badge&logo=docker&logoColor=white"></a>
</p>

![Listening: the station with its slideshow and text, the stations of the multiplex and of all multiplexes](docs/screenshots/listen.png)

| Ensemble | Spectrum |
|---|---|
| ![Ensemble: the multiplex, its capacity units and services](docs/screenshots/ensemble.png) | ![Spectrum, SNR, constellation, impulse response](docs/screenshots/spectrum.png) |

## Star History

<a href="https://www.star-history.com/#techkrzysiek/magicdab&Date">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=techkrzysiek/magicdab&type=Date&theme=dark">
    <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=techkrzysiek/magicdab&type=Date">
  </picture>
</a>


## Supported tuners and inputs

| Input | Notes |
|---|---|
| RTL-SDR USB sticks (RTL2832U with R820T / R828D) | e.g. RTL-SDR Blog V3 and V4, with bias-T; the packages bring librtlsdr 2.0.3 (osmocom), the Windows one also Zadig |
| rtl_tcp | an RTL-SDR on another computer, over the network |
| HackRF One | receive only, the amplifier always off, fixed LNA and VGA gains (`--hackrf`); Linux and Docker (libhackrf) |
| DTR-100 / wRadio C100 USB DAB+ dongle | the dongle demodulates by itself: no spectrum, TII, DX scan or gain control |
| ETI / EDI streams | `eti+tcp://host:port`, `edi+tcp://host:port`, mapped to channels |
| IQ recordings | `.uff`, raw u8 / s16 at 2.048 MS/s, played as channels |
| FM-DX-Tuner (TEF668x) | FM only, for DAB/FM service following |

## Running

- Port: 3222, DAB+ on a phone's keys (8080 is often FM-DX-Webserver's on
  the same computer); an installation from before 0.3.23 keeps 8080,
  `--listen` sets another one.
- Docker: `docker compose up -d` (see `docker-compose.yml`), web interface on
  port 3222 of the host (8080 inside the container); the admin password is
  logged at the first start. The image is `ghcr.io/techkrzysiek/magicdab`;
  Watchtower in the same file installs each new release at night.
- Windows: `build/windows.sh` builds `dist/windows/MagicDAB-<version>-windows-x64.zip`
  (see `build/windows/README.txt`).
- Linux (x64, arm64): `build/linux.sh` builds `dist/linux/MagicDAB-<version>-linux-<arch>.zip`
  for the architecture of the build host: one program with the interface
  inside and the RTL-SDR library next to it (see `build/linux/README.txt`).
- Android: `build/android.sh` builds the APK in Docker (see
  `docs/android.md`). The app opens the RTL-SDR stick on USB OTG itself (no
  driver app) and plays in the background with the screen off; its receiver
  is for the phone only: no tunnel, no server map, no chat.
- macOS (untested): `build/macos.sh` on a Mac builds one program for Apple
  silicon and Intel (see `build/macos/README.txt`).
- From source: `cargo build --release -p magicdab` (needs libfaad, libopus,
  librtlsdr) and `npm ci && npm run build` in `web/`.

## Releases

The Release workflow (each push to main that changes the program, a pushed
`v*` tag, or the Actions tab) builds the packages for Windows, Linux (x64,
and arm64 in a public repository) and macOS and the Docker image
(`ghcr.io/techkrzysiek/magicdab`, the server alone, x64 and arm64) and
publishes them with the release notes; a `v*` tag also builds the Android
APK (`.github/workflows/android.yml`, signed when the repository has the
signing secrets) and adds it to the release. Installed copies find new versions in
the update manifest of the latest release and install them from Settings >
Internet > Updates. The manifest is signed (`latest.json.sig`, Ed25519, with
the key in the `UPDATE_SIGNING_KEY` secret): a copy installs a package only
when the signature matches a public key built into it (`SIGNING_KEYS` in
`crates/server/src/update.rs`), so a fork publishing its own updates puts its
own key there.

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
