//! Public, read-only view of the receiver for plain HTTP clients: the
//! fm-dx-webserver compatible `/static_data` and `/api` endpoints (used by
//! tunnel services and server lists) and the Open Graph tags of the start page
//! (link previews in Discord & co.).

use serde::Serialize;
use serde_json::{json, Value};

#[derive(Clone, Debug, Default, Serialize)]
pub struct TxInfo {
    pub name: String,
    pub city: String,
    pub country: String,
    pub dist_km: Option<f64>,
    pub azimuth: Option<f64>,
    pub erp_kw: f32,
    pub pol: String,
    pub id: String,
}

/// Snapshot updated by the radio thread about once per second.
#[derive(Clone, Debug, Default)]
pub struct PublicInfo {
    pub station_name: String,
    pub description: String,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub channel: Option<String>,
    pub frequency_khz: u32,
    pub ensemble: String,
    pub eid: Option<u16>,
    pub ecc: Option<u8>,
    pub country_name: String,
    pub country_iso: String,
    pub sid: Option<u32>,
    pub label: String,
    pub short: String,
    pub pty: Option<u8>,
    pub dls: String,
    pub stereo: bool,
    pub bitrate: Option<u16>,
    pub codec: String,
    pub synced: bool,
    pub snr: f32,
    pub listeners: usize,
    /// FM frequencies (MHz) of the service from FIG 0/21.
    pub af: Vec<f64>,
    pub tx: Option<TxInfo>,
    /// Logo of the service in the logo store (file name, mime).
    pub logo: Option<(String, String)>,
    /// Search engines are kept out (Settings > Internet).
    pub no_index: bool,
    /// The chat is open (/chat).
    pub chat_enabled: bool,
    /// Entry on the public server map while listed (the map checks with it
    /// that the listed address leads here).
    pub map_id: Option<String>,
    /// A private server (Settings > Access): visitors give a password first,
    /// and the plain HTTP endpoints give only the station name.
    pub private: bool,
}

/// Crawlers of search engines and AI datasets (link previews of messengers
/// and social networks are not among them).
pub fn is_crawler(user_agent: &str) -> bool {
    const BOTS: [&str; 26] = [
        "googlebot",
        "google-inspectiontool",
        "googleother",
        "bingbot",
        "bingpreview",
        "slurp",
        "duckduckbot",
        "baiduspider",
        "yandex",
        "sogou",
        "exabot",
        "seznambot",
        "petalbot",
        "applebot",
        "ia_archiver",
        "archive.org_bot",
        "ahrefsbot",
        "semrushbot",
        "mj12bot",
        "dotbot",
        "gptbot",
        "ccbot",
        "claudebot",
        "bytespider",
        "amazonbot",
        "perplexitybot",
    ];
    let ua = user_agent.to_ascii_lowercase();
    BOTS.iter().any(|b| ua.contains(b))
}

pub fn static_data(p: &PublicInfo) -> Value {
    if p.private {
        return json!({"tunerName": p.station_name, "type": "dab", "private": true});
    }
    json!({
        "qthLatitude": p.lat.map(|v| format!("{v:.5}")).unwrap_or_default(),
        "qthLongitude": p.lon.map(|v| format!("{v:.5}")).unwrap_or_default(),
        "presets": [],
        "defaultTheme": "theme1",
        "bgImage": "",
        "rdsMode": false,
        "rdsTimeout": 0,
        "tunerName": p.station_name,
        "tunerDesc": if p.description.is_empty() { "DAB+ receiver (MagicDAB)".to_string() } else { p.description.clone() },
        "ant": {},
        // DAB specific
        "type": "dab",
        "software": format!("MagicDAB {}", crate::radio::VERSION),
        "mapId": p.map_id,
    })
}

pub fn api(p: &PublicInfo) -> Value {
    if p.private {
        return json!({"tunerName": p.station_name, "private": true});
    }
    let tx = p.tx.clone().unwrap_or_default();
    json!({
        "pi": p.sid.map(|s| format!("{:04X}", s & 0xFFFF)).unwrap_or_else(|| "?".into()),
        "freq": format!("{:.3}", p.frequency_khz as f64 / 1000.0),
        "sig": ((p.snr as f64) * 10.0).round() / 10.0,
        "sigRaw": "",
        "sigTop": ((p.snr as f64) * 10.0).round() / 10.0,
        "bw": 1536,
        "st": p.stereo,
        "stForced": false,
        "rds": p.synced,
        "ps": p.short,
        "lps": p.label,
        "tp": 0,
        "ta": 0,
        "ms": -1,
        "pty": p.pty.unwrap_or(0),
        "ecc": p.ecc,
        "af": p.af,
        "rt0": p.dls,
        "rt1": "",
        "rt_flag": "",
        "ps_errors": "",
        "agc": 0,
        "ant": 0,
        "txInfo": {
            "tx": tx.name,
            "pol": tx.pol,
            "erp": if tx.erp_kw > 0.0 { format!("{}", tx.erp_kw) } else { String::new() },
            "city": tx.city,
            "itu": tx.country,
            "dist": tx.dist_km.map(|d| format!("{d:.0}")).unwrap_or_default(),
            "azi": tx.azimuth.map(|a| format!("{a:.0}")).unwrap_or_default(),
            "id": tx.id,
            "reg": false,
            "pi": p.sid.map(|s| format!("{:04X}", s & 0xFFFF)).unwrap_or_default(),
        },
        "country_name": p.country_name,
        "country_iso": p.country_iso,
        "users": p.listeners,
        "rbds": false,
        // DAB specific
        "dab": {
            "channel": p.channel,
            "ensemble": p.ensemble,
            "eid": p.eid.map(|e| format!("{e:04X}")),
            "sid": p.sid.map(|s| format!("{s:X}")),
            "bitrate": p.bitrate,
            "codec": p.codec,
            "snr": ((p.snr as f64) * 10.0).round() / 10.0,
            "synced": p.synced,
        },
    })
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Plain text of a markdown description: links show their text, emphasis
/// markers are removed.
fn plain(md: &str) -> String {
    let mut out = String::new();
    let mut rest = md;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match (after.find("]("), after.find(')')) {
            (Some(a), Some(b)) if a < b => {
                out.push_str(&after[..a]);
                rest = &after[b + 1..];
            }
            _ => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.replace("**", "").replace("__", "")
}

/// Open Graph tags for link previews (Discord & co.): a stable description of
/// the server like fm-dx-webserver, not the current programme. `base` is the
/// public origin.
pub fn og_tags(p: &PublicInfo, base: &str) -> String {
    let name = if p.station_name.is_empty() { "MagicDAB".to_string() } else { p.station_name.clone() };
    // (a private server: its name, nothing it says about itself)
    let desc = if p.description.is_empty() || p.private {
        "DAB+ radio receiver in the browser".to_string()
    } else {
        format!("Server description: {}", plain(&p.description))
    };
    format!(
        concat!(
            "<meta property=\"og:title\" content=\"{title}\" />\n",
            "<meta property=\"og:type\" content=\"website\" />\n",
            "<meta property=\"og:image\" content=\"{base}/og.png\" />\n",
            "<meta property=\"og:description\" content=\"{desc}\" />\n",
            "<meta property=\"og:url\" content=\"{base}/\" />\n",
            "<meta name=\"description\" content=\"{desc}\" />\n",
            "<meta name=\"theme-color\" content=\"#8b5cf6\" />\n"
        ),
        title = esc(&if name == "MagicDAB" { name.clone() } else { format!("MagicDAB [{name}]") }),
        desc = esc(&desc),
        base = esc(base),
    )
}

/// Page title like fm-dx-webserver: "<name> - MagicDAB".
pub fn title(p: &PublicInfo) -> String {
    let name = if p.station_name.is_empty() || p.station_name == "MagicDAB" { String::new() } else { format!("{} - ", p.station_name) };
    format!("<title>{}MagicDAB</title>", esc(&name))
}
