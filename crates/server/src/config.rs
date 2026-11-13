//! Persistent settings.

use magicdab_dab::tii::TiiMode;
use magicdab_sdr::{DeviceConfig, GainMode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IdleScan {
    pub enabled: bool,
    /// Minutes without anyone listening before a scan starts.
    pub idle_minutes: u32,
    /// Minutes between scans while nobody listens; 0: one after another.
    pub interval_minutes: u32,
}

impl Default for IdleScan {
    fn default() -> Self {
        // (the band in a loop while the tuner is free: DX openings are caught)
        IdleScan { enabled: true, idle_minutes: 5, interval_minutes: 0 }
    }
}

/// Icecast style streams of the stations (icy.rs): /stream/<SId>, Ogg Opus.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct StreamSettings {
    pub enabled: bool,
    /// Stations streamed at once (each one decoded and encoded).
    pub max: u32,
    /// kbps
    pub bitrate: u32,
}

impl Default for StreamSettings {
    fn default() -> Self {
        StreamSettings { enabled: false, max: 2, bitrate: 128 }
    }
}

/// "Are you still listening?": a listener who did nothing for a while is
/// asked and, without an answer, disconnected (radio/presence.rs).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct IdleCheck {
    pub enabled: bool,
    /// Minutes without an action of the listener before the question.
    pub idle_minutes: u32,
    /// Seconds to answer it.
    pub answer_seconds: u32,
}

impl Default for IdleCheck {
    fn default() -> Self {
        IdleCheck { enabled: false, idle_minutes: 60, answer_seconds: 60 }
    }
}

/// Recordings of multiplexes heard on a channel for the first time (DX), as
/// ETI files in data/dxrec (dxrec.rs).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct DxRecordConfig {
    /// Off unless the admin switches it on: a minute is about 15 MB.
    pub enabled: bool,
    /// Longest recording.
    pub minutes: u32,
    /// The folder is kept under this; the oldest recordings go first.
    pub max_mb: u32,
    /// A scan also records every multiplex from farther than `far_km` (its
    /// nearest transmitter heard, radio/dxrec.rs); on unless switched off,
    /// also on installations from before it, but not in the Android app (the
    /// phone's storage).
    pub far: bool,
    pub far_km: u32,
}

impl Default for DxRecordConfig {
    fn default() -> Self {
        DxRecordConfig { enabled: false, minutes: 1, max_mb: 2048, far: !cfg!(target_os = "android"), far_km: 200 }
    }
}

/// A multiplex (ECC and EId, "E23ABC") or a service (SId, "1234") left out of
/// the change log, with its name as it was muted.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Muted {
    pub id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyMute {
    pub ensembles: Vec<Muted>,
    pub services: Vec<Muted>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub station_name: String,
    /// Server description (markdown), shown in the UI, link previews and /static_data.
    pub description: String,
    pub device: DeviceConfig,
    pub gain: GainMode,
    pub ppm: i32,
    /// "salt$sha256hex" of the admin password.
    pub admin_password_hash: Option<String>,
    /// When locked only the admin may change the multiplex.
    pub tuner_locked: bool,
    /// A receiver kept on one channel and on one station (hex SId), e.g. one
    /// whose stream another multiplex takes: set in the interface, or by
    /// --channel / --service.
    pub lock_channel: Option<String>,
    pub lock_service: Option<String>,
    /// The --channel / --service last applied ("12C/4A91"): a start with
    /// other ones applies those, with the same ones the settings stay as the
    /// admin left them.
    pub locks_from_args: Option<String>,
    /// Anyone may listen. Off (a private server): a visitor gives the admin
    /// or the tuning password first and gets nothing before (radio/gate.rs);
    /// the server is not listed on the public map.
    pub public_server: bool,
    /// Listeners who do nothing for a while are asked whether they still listen.
    pub idle_check: IdleCheck,
    pub location: Option<Location>,
    pub last_channel: Option<String>,
    pub last_service: Option<u32>,
    pub idle_scan: IdleScan,
    pub opus_bitrate: u32,
    pub tii_mode: TiiMode,
    pub tii_log: bool,
    pub scan_channels: Vec<String>,
    /// Maximum number of simultaneously decoded services (CPU protection).
    pub max_services: usize,
    /// Jitter buffer of ETI / EDI streams (ms; 0: none), as ODR-DabMod's
    /// delay: frames reach the receiver evenly however TCP brings them.
    pub eti_delay_ms: u32,
    /// Decode SPI (logos, service information) from the ensemble.
    pub spi: bool,
    /// Use the internet (RadioDNS) for logos and service information.
    pub internet: bool,
    /// Follow RadioVIS (slideshow over IP via RadioDNS) of the selected services.
    pub radio_vis: bool,
    /// Decode DLS and slideshow of all services in the background.
    pub background_pad: bool,
    /// Discord webhook URL for ensemble change notifications.
    pub discord_webhook: String,
    /// Channels fed by ETI / EDI streams instead of the tuner.
    pub channel_maps: Vec<ChannelMap>,
    /// Listeners choose their own multiplex among the mapped channels: a
    /// receiver of its own (side slot) instead of moving the main one.
    pub listener_multiplex: bool,
    /// Side slots at once (recordings: at most two of them).
    pub listener_multiplex_limit: usize,
    /// Public tunnel (frp).
    pub tunnel: crate::tunnel::TunnelConfig,
    /// FM tuner (FM-DX-Tuner, e.g. the TEF668x headless USB tuner).
    pub fm: FmConfig,
    /// DAB/FM linking of the service heard.
    pub linking: LinkingConfig,
    /// Stations as Icecast style streams.
    pub streams: StreamSettings,
    /// Address of the update manifest (latest.json); empty: the one built in.
    pub update_url: String,
    /// Install updates by themselves when no one is listening.
    pub update_auto: bool,
    /// Open the web interface in the browser when the program starts (Windows).
    pub open_browser: bool,
    /// The port this installation listens on without --listen (default_port);
    /// 0 until the first start sets it.
    pub listen_port: u16,
    /// Keep search engines out: noindex/nofollow, robots.txt, crawlers refused.
    pub no_index: bool,
    /// Chat of the listeners (/chat, compatible with FM-DX-Webserver).
    pub chat_enabled: bool,
    /// Where the source code of this (possibly modified) version is (AGPL
    /// section 13: users over the network get it); empty: the built-in one.
    pub source_url: String,
    /// Multiplexes and services the change log does not report (Changes view).
    pub notify_mute: NotifyMute,
    /// ETI recordings of multiplexes heard for the first time (DX).
    pub dx_record: DxRecordConfig,
    /// Bans, refused networks, the tuning password and the channels of the listeners.
    pub access: AccessConfig,
    /// Listing on the public map of MagicDAB servers (off by default).
    pub server_map: crate::servermap::MapConfig,
    /// Picture behind the interface of every listener: an http(s) address, or
    /// one uploaded by the admin (`/background/<hash>.<ext>`, background.rs);
    /// empty: none.
    pub background_image: String,
    /// The first-run wizard has been neither finished nor skipped: only a new
    /// installation (no settings file) starts with it.
    pub setup_pending: bool,
}

/// A banned address or network.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Ban {
    /// Address or network (CIDR): 203.0.113.7, 2001:db8::/48.
    pub address: String,
    /// Why (for the admin; the banned client is not told).
    pub reason: String,
    /// Unix time of the ban.
    pub since: i64,
    /// Unix time it ends; none: until lifted.
    pub until: Option<i64>,
    /// Banned by the server itself (a flood of commands or chat messages).
    pub auto: bool,
}

impl Ban {
    pub fn active(&self, now: i64) -> bool {
        self.until.is_none_or(|u| now < u)
    }
}

/// Who may use the receiver, and what listeners may tune to.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct AccessConfig {
    pub bans: Vec<Ban>,
    /// Refuse Tor exit nodes.
    pub block_tor: bool,
    /// Refuse the addresses of these autonomous systems.
    pub blocked_asns: Vec<u32>,
    /// Hours of the automatic ban of a flooding client (0: none).
    pub auto_ban_hours: u32,
    /// Channels listeners may tune to (empty: all); the admin is not limited.
    pub allowed_channels: Vec<String>,
    /// "salt$sha256hex" of the password with which listeners may tune while
    /// the tuner is locked (without becoming admin).
    pub tune_password_hash: Option<String>,
}

impl Default for AccessConfig {
    fn default() -> Self {
        AccessConfig {
            bans: Vec::new(),
            block_tor: false,
            blocked_asns: Vec::new(),
            auto_ban_hours: 24,
            allowed_channels: Vec::new(),
            tune_password_hash: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct LinkingConfig {
    pub enabled: bool,
    pub mode: crate::linking::Mode,
    /// Longest delay between FM and DAB the FM buffer absorbs (ms).
    pub max_delay_ms: u32,
    /// FM brought to the loudness of DAB (off: as the tuner gives it).
    pub match_level: bool,
}

impl Default for LinkingConfig {
    fn default() -> Self {
        LinkingConfig { enabled: true, mode: Default::default(), max_delay_ms: 5000, match_level: true }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct FmConfig {
    pub enabled: bool,
    /// Serial port ("" finds the tuner).
    pub port: String,
    /// ALSA capture device of the tuner's audio ("" = the tuner's USB audio).
    pub audio: String,
    /// Last frequency (kHz).
    pub khz: u32,
    /// Antennas on the tuner's antenna switch (1-4).
    pub antennas: u8,
    /// Last antenna.
    pub antenna: u8,
    /// Set since the tuner is off by default (0.3.23): an older settings file
    /// without it gets the tuner turned off once (Store::load).
    #[serde(default)]
    pub off_by_default: bool,
}

impl Default for FmConfig {
    fn default() -> Self {
        // (off until the admin turns it on: it would look for the tuner on every serial port)
        FmConfig { enabled: false, port: String::new(), audio: String::new(), khz: 0, antennas: 1, antenna: 0, off_by_default: true }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChannelMap {
    pub channel: String,
    /// edi+tcp://host:port or eti+tcp://host:port, or file:///path/recording.uff
    /// (raw IQ, played in a loop as the tuner of this channel)
    pub url: String,
}

impl ChannelMap {
    /// Path of a raw IQ recording mapped to the channel.
    pub fn file(&self) -> Option<&str> {
        let p = self.url.strip_prefix("file://").or_else(|| self.url.strip_prefix("file:"))?;
        (!p.is_empty()).then_some(p)
    }
}

impl Default for Settings {
    fn default() -> Self {
        let s = Settings {
            station_name: "MagicDAB".into(),
            description: String::new(),
            device: DeviceConfig::default(),
            gain: GainMode::Software,
            ppm: 0,
            admin_password_hash: None,
            // (everyone may change the multiplex until the admin locks it)
            tuner_locked: false,
            lock_channel: None,
            lock_service: None,
            locks_from_args: None,
            public_server: true,
            idle_check: IdleCheck::default(),
            location: None,
            last_channel: None,
            last_service: None,
            idle_scan: IdleScan::default(),
            opus_bitrate: 192_000,
            tii_mode: TiiMode::Sensitive,
            tii_log: false,
            scan_channels: magicdab_dab::consts::BAND_III.iter().filter(|(n, _)| !n.ends_with('N')).map(|(n, _)| n.to_string()).collect(),
            max_services: 8,
            eti_delay_ms: 1000,
            spi: true,
            internet: true,
            radio_vis: true,
            background_pad: true,
            discord_webhook: String::new(),
            channel_maps: Vec::new(),
            listener_multiplex: true,
            listener_multiplex_limit: 4,
            tunnel: Default::default(),
            fm: Default::default(),
            linking: Default::default(),
            streams: Default::default(),
            // packages may come with their address built in
            update_url: String::new(),
            update_auto: false,
            open_browser: true,
            listen_port: 0,
            no_index: false,
            chat_enabled: true,
            source_url: String::new(),
            notify_mute: NotifyMute::default(),
            dx_record: DxRecordConfig::default(),
            access: Default::default(),
            server_map: Default::default(),
            background_image: String::new(),
            // (settings of older versions lack the field: no wizard for them)
            setup_pending: false,
        };
        if cfg!(target_os = "android") {
            return Settings::android(s);
        }
        s
    }
}

impl Settings {
    /// The Android app: one listener on a phone with a battery, DAB only.
    /// Scans while idle and the DLS and slides of every station (the whole
    /// multiplex decoded) cost battery, the chat has no one to talk to, DX
    /// recordings take the phone's storage (DxRecordConfig::default; all can
    /// be switched on in the settings), there is no FM tuner.
    fn android(s: Settings) -> Settings {
        let mut s = Settings { idle_scan: IdleScan { enabled: false, ..s.idle_scan }, background_pad: false, ..s };
        s.phone_only(true);
        s
    }

    /// The Android app (`phone`): a receiver of its own for the one person
    /// holding the phone, with the RTL-SDR stick on its USB and the interface
    /// only on the phone itself. Nothing is made public (no tunnel, no listing
    /// on the map of servers, no chat), there is no other input (no rtl_tcp,
    /// ETI/EDI streams or FM tuner) and no first-run wizard. Applied to the
    /// settings as loaded and to every change of them; true when it changed
    /// something.
    pub fn phone_only(&mut self, phone: bool) -> bool {
        if !phone {
            return false;
        }
        let before = serde_json::to_value(&*self).ok();
        if !matches!(self.device, DeviceConfig::RtlSdr { .. }) {
            self.device = DeviceConfig::RtlSdr { index: 0, ppm: 0, bias_tee: false };
        }
        self.tunnel.enabled = false;
        self.server_map.enabled = false;
        self.chat_enabled = false;
        self.channel_maps.clear();
        self.listener_multiplex = false;
        self.fm.enabled = false;
        self.linking.enabled = false;
        self.update_auto = false;
        self.setup_pending = false;
        serde_json::to_value(&*self).ok() != before
    }
}

/// A background picture address as given in the settings: http(s), or a
/// picture uploaded to this server (`/background/<hash>.<ext>`), and nothing
/// that could end the CSS `url("…")` it is put in. `Some("")` clears it.
pub fn background_url(v: &str) -> Option<String> {
    let v = v.trim();
    if v.is_empty() {
        return Some(String::new());
    }
    if let Some(name) = v.strip_prefix(crate::background::URL_PREFIX) {
        return crate::background::is_file_name(name).then(|| v.to_string());
    }
    let lower = v.to_ascii_lowercase();
    let rest = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://"))?;
    let host_ok = rest.split(['/', '?', '#']).next().is_some_and(|h| !h.is_empty() && !h.starts_with(':'));
    let chars_ok = v.chars().all(|c| !c.is_whitespace() && !c.is_control() && !matches!(c, '"' | '\'' | '\\' | '<' | '>' | '`'));
    (host_ok && chars_ok && v.len() <= 2048).then(|| v.to_string())
}

pub fn hash_password(password: &str) -> String {
    let salt: [u8; 8] = rand::random();
    let salt_hex: String = salt.iter().map(|b| format!("{b:02x}")).collect();
    format!("{salt_hex}${}", digest(&salt_hex, password))
}

fn digest(salt: &str, password: &str) -> String {
    let mut h = Sha256::new();
    h.update(salt.as_bytes());
    h.update(password.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    match hash.split_once('$') {
        Some((salt, d)) => digest(salt, password) == d,
        None => false,
    }
}

/// The port of a new installation: 3222, DAB+ on a phone's keys (D 3, A 2,
/// B 2, plus a 2); 8080 is FM-DX-Webserver's, often on the same computer.
pub const DEFAULT_PORT: u16 = 3222;
/// The port of the installations from before 0.3.23 (their address stays in
/// bookmarks and port forwards).
pub const OLD_DEFAULT_PORT: u16 = 8080;

/// The port to listen on without --listen: the one the installation uses
/// (settings), 8080 for one from before 0.3.23, DEFAULT_PORT for a new one.
pub fn default_port(dir: &Path) -> u16 {
    match std::fs::read_to_string(dir.join("settings.json")) {
        Ok(s) => serde_json::from_str::<serde_json::Value>(&s)
            .ok()
            .and_then(|v| v["listenPort"].as_u64())
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| *p > 0)
            .unwrap_or(OLD_DEFAULT_PORT),
        Err(_) => DEFAULT_PORT,
    }
}

pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn new(dir: &Path) -> Self {
        Store { path: dir.join("settings.json") }
    }

    pub fn load(&self) -> Settings {
        // (a file of an older version, readable by everyone, until it is saved again)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::metadata(&self.path).is_ok_and(|m| m.permissions().mode() & 0o077 != 0) {
                let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
            }
        }
        match std::fs::read_to_string(&self.path) {
            Ok(s) => serde_json::from_str(&s).map(Self::fm_off_once).unwrap_or_else(|e| {
                // (kept aside: the next save would overwrite the passwords and
                // the tunnel token in it)
                let aside = self.path.with_extension(format!("json.invalid-{}", chrono::Local::now().format("%Y%m%d-%H%M%S")));
                let _ = std::fs::rename(&self.path, &aside);
                log::warn!("settings.json invalid ({e}), using defaults; the file is kept as {}", aside.display());
                Settings::default()
            }),
            // a new installation: the admin is walked through the setup
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings { setup_pending: true, ..Settings::default() },
            Err(_) => Settings::default(),
        }
    }

    /// The FM tuner is off by default since 0.3.23, also where an older version
    /// left it on: once, the admin turns it on again in Settings > FM.
    fn fm_off_once(mut s: Settings) -> Settings {
        if !s.fm.off_by_default {
            if s.fm.enabled {
                log::info!("FM tuner: off by default from this version on (Settings > FM and linking turns it on)");
            }
            s.fm.enabled = false;
            s.fm.off_by_default = true;
        }
        s
    }

    pub fn save(&self, s: &Settings) {
        let tmp = self.path.with_extension("json.tmp");
        match serde_json::to_string_pretty(s) {
            Ok(txt) => {
                // on the disk before it replaces the old file: a power cut must
                // not leave an empty settings file (Windows does not see to that)
                // (only its user reads it: the passwords' hashes, the tunnel's token)
                let _ = std::fs::remove_file(&tmp);
                let mut open = std::fs::OpenOptions::new();
                open.write(true).create_new(true);
                #[cfg(unix)]
                std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
                let written = open.open(&tmp).and_then(|mut f| {
                    use std::io::Write;
                    f.write_all(txt.as_bytes())?;
                    f.sync_all()
                });
                match written.and_then(|_| std::fs::rename(&tmp, &self.path)) {
                    Ok(()) => {}
                    Err(e) => log::warn!("cannot save settings: {e}"),
                }
            }
            Err(e) => log::warn!("cannot serialise settings: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phone_has_its_stick_and_nothing_public() {
        let mut s = Settings {
            device: DeviceConfig::RtlTcp { host: "127.0.0.1".into(), port: 1234 },
            chat_enabled: true,
            setup_pending: true,
            ..Settings::default()
        };
        s.tunnel.enabled = true;
        s.server_map.enabled = true;
        assert!(!s.clone().phone_only(false));
        assert!(s.phone_only(true));
        assert!(matches!(s.device, DeviceConfig::RtlSdr { .. }));
        assert!(!s.tunnel.enabled && !s.server_map.enabled && !s.chat_enabled && !s.setup_pending);
        // (once is enough)
        assert!(!s.phone_only(true));
    }

    #[test]
    fn a_new_installation_gets_the_new_port_an_old_one_keeps_8080() {
        let dir = std::env::temp_dir().join(format!("magicdab-port-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(default_port(&dir), DEFAULT_PORT);
        std::fs::write(dir.join("settings.json"), r#"{"stationName": "x"}"#).unwrap();
        assert_eq!(default_port(&dir), OLD_DEFAULT_PORT);
        std::fs::write(dir.join("settings.json"), r#"{"listenPort": 9123}"#).unwrap();
        assert_eq!(default_port(&dir), 9123);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn only_its_user_reads_the_settings() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("magicdab-mode-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::new(&dir);
        let mode = || std::fs::metadata(dir.join("settings.json")).unwrap().permissions().mode() & 0o777;
        // written by an older version: everyone could read it
        std::fs::write(dir.join("settings.json"), r#"{"stationName": "x"}"#).unwrap();
        std::fs::set_permissions(dir.join("settings.json"), std::fs::Permissions::from_mode(0o644)).unwrap();
        store.load();
        assert_eq!(mode(), 0o600);
        store.save(&Settings::default());
        assert_eq!(mode(), 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_fm_tuner_goes_off_once_after_the_update() {
        let dir = std::env::temp_dir().join(format!("magicdab-fm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::new(&dir);
        // a file of an older version: the tuner on, no mark
        std::fs::write(dir.join("settings.json"), r#"{"fm": {"enabled": true, "port": "COM5", "khz": 98100}}"#).unwrap();
        let s = store.load();
        assert!(!s.fm.enabled && s.fm.off_by_default);
        assert_eq!((s.fm.port.as_str(), s.fm.khz), ("COM5", 98100));
        // turned on again by the admin: it stays on
        let mut s = s;
        s.fm.enabled = true;
        store.save(&s);
        assert!(store.load().fm.enabled);
        // a new installation: off
        assert!(!Settings::default().fm.enabled);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn background_addresses() {
        let ok = |v: &str| background_url(v);
        assert_eq!(ok(" https://example.net/files/station.jpg "), Some("https://example.net/files/station.jpg".into()));
        assert_eq!(ok("HTTP://example.org/a_(1).png?x=1#y"), Some("HTTP://example.org/a_(1).png?x=1#y".into()));
        // empty clears the picture
        assert_eq!(ok(""), Some(String::new()));
        assert_eq!(ok("  "), Some(String::new()));
        // a picture uploaded to this server (background.rs)
        assert_eq!(ok("/background/0123456789abcdef0123456789abcdef.jpg"), Some("/background/0123456789abcdef0123456789abcdef.jpg".into()));
        assert_eq!(
            ok(" /background/0123456789abcdef0123456789abcdef.webp "),
            Some("/background/0123456789abcdef0123456789abcdef.webp".into())
        );
        for bad in [
            "/background/",
            "/background/../settings.json",
            "/background/0123456789abcdef0123456789abcdef.svg",
            "/background/x.jpg",
            "/background/0123456789abcdef0123456789abcdef.jpg\")",
            "background/0123456789abcdef0123456789abcdef.jpg",
        ] {
            assert_eq!(ok(bad), None, "{bad:?}");
        }
        for bad in [
            "javascript:alert(1)",
            "data:image/png;base64,AAAA",
            "//example.org/a.jpg",
            "/files/a.jpg",
            "ftp://example.org/a.jpg",
            "https://",
            "https:///a.jpg",
            "https://:443/a.jpg",
            "https://a b/c.jpg",
            "https://x.org/a.jpg\"); background: red",
            "https://x.org/a.jpg'",
            "https://x.org/a\\.jpg",
            "https://x.org/<script>",
            "https://x.org/a\n.jpg",
        ] {
            assert_eq!(ok(bad), None, "{bad:?}");
        }
        assert_eq!(ok(&format!("https://x.org/{}", "a".repeat(3000))), None);
    }

    #[test]
    fn only_a_new_installation_starts_with_the_setup() {
        let dir = std::env::temp_dir().join(format!("magicdab-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::new(&dir);
        // no settings file: a new installation
        let mut s = store.load();
        assert!(s.setup_pending);
        // saved at the start, it stays pending until finished or skipped
        store.save(&s);
        assert!(store.load().setup_pending);
        s.setup_pending = false;
        store.save(&s);
        assert!(!store.load().setup_pending);
        // settings of an older version (without the field): set up long ago
        std::fs::write(dir.join("settings.json"), r#"{"stationName": "Example", "tunerLocked": false}"#).unwrap();
        let old = store.load();
        assert!(!old.setup_pending);
        assert_eq!(old.station_name, "Example");
        assert!(old.background_image.is_empty());
        // a public server, as every one was; the password of the tunnel's HTTP
        // authentication of earlier versions is dropped without complaint
        assert!(old.public_server && !old.idle_check.enabled);
        std::fs::write(
            dir.join("settings.json"),
            r#"{"tunerLocked": true, "tunnel": {"enabled": true, "httpUser": "dx", "httpPassword": "pw"}, "publicServer": false}"#,
        )
        .unwrap();
        let old = store.load();
        assert!(old.tunnel.enabled && old.tuner_locked && !old.public_server);
        store.save(&old);
        assert!(!std::fs::read_to_string(dir.join("settings.json")).unwrap().contains("httpUser"));
        // a new installation leaves the tuner to everyone
        assert!(!Settings::default().tuner_locked);
        // nor is a damaged file a new installation; it is kept aside, not
        // overwritten by the next save
        std::fs::write(dir.join("settings.json"), "{").unwrap();
        assert!(!store.load().setup_pending);
        let aside: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("settings.json.invalid-"))
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(std::fs::read_to_string(aside[0].path()).unwrap(), "{");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
