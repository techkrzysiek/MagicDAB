//! Conversion of the receiver's ensemble database into the JSON model used by
//! the web UI (names resolved, linking information merged per service).

use magicdab_dab::ensemble::{Component, Ensemble, FrequencyInfo, LinkedId, Service};
use magicdab_dab::tables;
use serde_json::{json, Value};
use std::collections::HashMap;

pub type LongLabels = HashMap<(u8, u32, u8), String>;

fn hex16(v: u32) -> String {
    format!("{v:04X}")
}

fn rm_name(rm: u8) -> &'static str {
    match rm {
        0 | 1 => "DAB",
        6 => "DRM",
        8 => "FM (RDS)",
        9 => "FM",
        10 | 12 => "AM",
        14 => "AMSS",
        _ => "?",
    }
}

fn component_type(c: &Component) -> String {
    match c.tmid {
        0 => {
            if c.ctype == 63 {
                "DAB+ audio".into()
            } else {
                "DAB audio".into()
            }
        }
        1 => format!("Data stream ({})", tables::dscty_name(c.ctype)),
        3 => format!("Packet data ({})", tables::dscty_name(c.ctype)),
        _ => "FIDC".into(),
    }
}

fn frequencies_for(fi: &[FrequencyInfo], id: u32, rms: &[u8]) -> Vec<u32> {
    let mut v: Vec<u32> =
        fi.iter().filter(|f| f.id == id && rms.contains(&f.rm)).flat_map(|f| f.frequencies.iter().map(|e| e.khz)).collect();
    v.sort();
    v.dedup();
    v
}

fn ensembles_with_freqs(ens: &Ensemble, eids: &[u16]) -> Vec<Value> {
    eids.iter()
        .map(|&e| {
            let f = frequencies_for(&ens.frequency_info, e as u32, &[0, 1]);
            json!({
                "eid": hex16(e as u32),
                "frequencies": f,
                "channels": f.iter().map(|&k| magicdab_dab::consts::channel_name(k).unwrap_or("")).collect::<Vec<_>>(),
            })
        })
        .collect()
}

fn freq_list(khz: &[u32], dab: bool) -> Vec<Value> {
    khz.iter()
        .map(|&k| {
            json!({
                "khz": k,
                "channel": if dab { magicdab_dab::consts::channel_name(k) } else { None },
            })
        })
        .collect()
}

/// Service following information of one service, split by origin:
/// `links` from FIG 0/6 (service linking), `frequencies` from FIG 0/21
/// (frequency information) and `ensembles` from FIG 0/24 (other ensembles).
fn linking(ens: &Ensemble, sid: u32) -> Value {
    let mut links = Vec::new();
    let mut fm_ids: Vec<u32> = vec![sid & 0xFFFF];
    let mut dab_sids: Vec<u32> = Vec::new();
    for set in ens.linkage.iter().filter(|l| l.key_sid == Some(sid) || l.ids.iter().any(|i| i.kind == 0 && i.id == sid)) {
        // the key service is kept apart from the ids: it is a DAB link of the others
        let key = set.key_sid.filter(|&k| k != sid).map(|k| LinkedId { kind: 0, ecc: None, id: k });
        for id in key.iter().chain(&set.ids) {
            if id.kind == 0 && id.id == sid {
                continue;
            }
            let kind = match id.kind {
                0 => "DAB",
                1 => "FM",
                2 => "AM/FM",
                _ => "DRM/AMSS",
            };
            match id.kind {
                0 => dab_sids.push(id.id),
                1 => fm_ids.push(id.id),
                _ => {}
            }
            links.push(json!({
                "kind": kind,
                "id": if id.kind == 3 { format!("{:06X}", id.id) } else { hex16(id.id) },
                "label": if id.kind == 0 { ens.services.get(&id.id).and_then(|s| s.label.as_ref()).map(|l| l.text.clone()) } else { None },
                "ecc": id.ecc.map(|e| format!("{e:02X}")),
                "lsn": format!("{:03X}", set.lsn),
                "hard": set.hard,
                "active": set.active,
                "international": set.international,
                "oe": set.oe,
            }));
        }
    }
    fm_ids.sort();
    fm_ids.dedup();
    let mut frequencies = Vec::new();
    for pi in &fm_ids {
        let f = frequencies_for(&ens.frequency_info, *pi, &[8, 9]);
        if !f.is_empty() {
            frequencies.push(json!({"kind": "FM", "ref": hex16(*pi), "frequencies": freq_list(&f, false)}));
        }
    }
    // other ensembles carrying this service or a linked DAB service (FIG 0/24) and their frequencies
    let mut oe_eids: Vec<u16> = Vec::new();
    let mut ensembles = Vec::new();
    for s in std::iter::once(sid).chain(dab_sids.iter().copied()) {
        for o in ens.oe_services.iter().filter(|o| o.sid == s) {
            for &e in &o.eids {
                if !oe_eids.contains(&e) {
                    oe_eids.push(e);
                    let f = frequencies_for(&ens.frequency_info, e as u32, &[0, 1]);
                    ensembles.push(json!({
                        "eid": hex16(e as u32),
                        "via": hex16(s),
                        "frequencies": freq_list(&f, true),
                    }));
                    if !f.is_empty() {
                        frequencies.push(json!({"kind": "DAB", "ref": hex16(e as u32), "frequencies": freq_list(&f, true)}));
                    }
                }
            }
        }
    }
    for f in ens.frequency_info.iter().filter(|f| (f.rm == 6 || f.rm == 14) && dab_sids.contains(&f.id)) {
        let khz: Vec<u32> = f.frequencies.iter().map(|e| e.khz).collect();
        frequencies.push(
            json!({"kind": if f.rm == 6 { "DRM" } else { "AMSS" }, "ref": format!("{:06X}", f.id), "frequencies": freq_list(&khz, false)}),
        );
    }
    json!({ "links": links, "frequencies": frequencies, "ensembles": ensembles })
}

pub fn service(ens: &Ensemble, labels: &LongLabels, s: &Service) -> Value {
    let p = s.primary();
    let sc = p.and_then(|c| c.subch_id).filter(|_| p.map(|c| c.tmid != 3).unwrap_or(false)).and_then(|id| ens.subchannels.get(&id));
    let sc = sc.or_else(|| p.and_then(|c| c.subch_id).and_then(|id| ens.subchannels.get(&id)));
    let ecc = s.ecc.or(ens.ecc);
    let country = ecc.and_then(|e| tables::country(e, s.country_id()));
    let kind = if s.is_data { 5 } else { 1 };
    let components: Vec<Value> = s
        .components
        .iter()
        .map(|c| {
            let csc = c.subch_id.and_then(|id| ens.subchannels.get(&id));
            json!({
                "scids": c.scids,
                "tmid": c.tmid,
                "type": component_type(c),
                "ctype": c.ctype,
                "subch": c.subch_id,
                "scid": c.scid,
                "primary": c.primary,
                "ca": c.ca,
                "label": c.label.as_ref().map(|l| l.text.clone()),
                "short": c.label.as_ref().map(|l| l.short.clone()),
                "long": c.scids.and_then(|i| labels.get(&(4, s.sid, i))).cloned(),
                "language": c.language.map(tables::language_name),
                "packetAddress": c.packet_address,
                "bitrate": csc.map(|x| x.bitrate),
                "protection": csc.map(|x| x.protection_label.clone()),
                "userApps": c.user_apps.iter().map(|u| json!({
                    "type": u.ua_type,
                    "name": tables::user_app_name(u.ua_type),
                    "xpadAppType": u.xpad_app_type,
                    "dscty": u.dscty,
                    "data": u.data.iter().map(|b| format!("{b:02X}")).collect::<String>(),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let asu: Vec<&str> = (0..11).filter(|b| s.asu & (1 << b) != 0).map(|b| tables::ANNOUNCEMENT_TYPES[b as usize]).collect();
    json!({
        "sid": s.sid,
        "sidHex": if s.is_data { format!("{:08X}", s.sid) } else { hex16(s.sid) },
        "ecc": ecc,
        "country": country.map(|c| c.0),
        "countryName": country.map(|c| c.1),
        "label": s.label.as_ref().map(|l| l.text.clone()).unwrap_or_default(),
        "short": s.label.as_ref().map(|l| l.short.clone()).unwrap_or_default(),
        "long": labels.get(&(kind, s.sid, 0)),
        "pty": s.pty,
        "ptyName": s.pty.map(tables::pty_name),
        "ptyDynamic": s.pty_dynamic,
        "language": s.language.or_else(|| p.and_then(|c| c.language)).map(tables::language_name),
        "isAudio": s.is_audio(),
        "isData": s.is_data || !s.is_audio(),
        "ca": s.ca_id != 0,
        "codec": p.map(|c| if c.tmid == 0 { if c.ctype == 63 { "DAB+" } else { "DAB" } } else { "Data" }),
        "ascty": p.map(|c| c.ctype),
        "bitrate": sc.map(|x| x.bitrate),
        "subchannel": sc.map(|x| x.id),
        "protection": sc.map(|x| x.protection_label.clone()),
        "codeRate": sc.map(|x| x.code_rate.clone()),
        "startCu": sc.map(|x| x.start),
        "sizeCu": sc.map(|x| x.size),
        "components": components,
        "asu": asu,
        "clusters": s.clusters,
        "linking": linking(ens, s.sid),
    })
}

pub fn ensemble(ens: &Ensemble, labels: &LongLabels, channel: Option<&str>, frequency: u32) -> Value {
    let Some(eid) = ens.eid else {
        return json!({ "valid": false, "channel": channel, "frequency": frequency });
    };
    let country = ens.ecc.and_then(|e| tables::country(e, (eid >> 12) as u8));
    let mut services: Vec<Value> = ens.services.values().map(|s| service(ens, labels, s)).collect();
    services.sort_by(|a, b| a["label"].as_str().unwrap_or("").to_lowercase().cmp(&b["label"].as_str().unwrap_or("").to_lowercase()));
    let mut audio_cu = 0u32;
    let mut data_cu = 0u32;
    let subchannels: Vec<Value> = ens
        .subchannels
        .values()
        .map(|sc| {
            let user = ens.component_for_subchannel(sc.id).or_else(|| {
                ens.services.values().flat_map(|s| s.components.iter().map(move |c| (s, c))).find(|(_, c)| c.subch_id == Some(sc.id))
            });
            let is_audio = user.map(|(_, c)| c.tmid == 0).unwrap_or(false);
            if is_audio {
                audio_cu += sc.size as u32;
            } else {
                data_cu += sc.size as u32;
            }
            let users: Vec<String> = ens
                .services
                .values()
                .filter(|s| s.components.iter().any(|c| c.subch_id == Some(sc.id)))
                .map(|s| s.label.as_ref().map(|l| l.text.clone()).unwrap_or_else(|| hex16(s.sid)))
                .collect();
            json!({
                "id": sc.id,
                "start": sc.start,
                "size": sc.size,
                "bitrate": sc.bitrate,
                "protection": sc.protection_label,
                "codeRate": sc.code_rate,
                "fec": sc.fec_scheme,
                "audio": is_audio,
                "content": user.map(|(_, c)| component_type(c)),
                "services": users,
            })
        })
        .collect();
    let used = ens.used_cus();
    let fi: Vec<Value> = ens
        .frequency_info
        .iter()
        .map(|f| {
            json!({
                "id": if f.rm == 6 || f.rm == 14 { format!("{:06X}", f.id) } else { hex16(f.id) },
                "rm": f.rm,
                "type": rm_name(f.rm),
                "oe": f.oe,
                "regionId": f.region_id,
                "continuity": f.continuity,
                "frequencies": f.frequencies.iter().map(|e| json!({
                    "khz": e.khz,
                    "channel": if f.rm <= 1 { magicdab_dab::consts::channel_name(e.khz) } else { None },
                    "adjacent": e.adjacent,
                    "mode": e.mode,
                })).collect::<Vec<_>>(),
                "label": if f.rm <= 1 && !f.oe && Some(f.id as u16) == ens.eid { ens.label.as_ref().map(|l| l.text.clone()) } else { None },
            })
        })
        .collect();
    let linkage: Vec<Value> = ens
        .linkage
        .iter()
        .map(|l| {
            json!({
                "lsn": format!("{:03X}", l.lsn),
                "active": l.active,
                "hard": l.hard,
                "international": l.international,
                "oe": l.oe,
                "keySid": l.key_sid.map(hex16),
                "keyLabel": l.key_sid.and_then(|k| ens.services.get(&k)).and_then(|s| s.label.as_ref()).map(|l| l.text.clone()),
                "ids": l.ids.iter().map(|i| json!({
                    "kind": match i.kind { 0 => "DAB", 1 => "RDS PI", 2 => "AM/FM", _ => "DRM/AMSS" },
                    "id": hex16(i.id),
                    "ecc": i.ecc.map(|e| format!("{e:02X}")),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let oe: Vec<Value> = ens
        .oe_services
        .iter()
        .map(|o| {
            json!({
                "sid": hex16(o.sid),
                "label": ens.services.get(&o.sid).and_then(|s| s.label.as_ref()).map(|l| l.text.clone()),
                "ensembles": ensembles_with_freqs(ens, &o.eids),
            })
        })
        .collect();
    let announcements: Vec<Value> = ens
        .announcements
        .values()
        .map(|a| {
            let types: Vec<&str> = (0..11).filter(|b| a.asw & (1 << b) != 0).map(|b| tables::ANNOUNCEMENT_TYPES[b as usize]).collect();
            json!({ "cluster": a.cluster, "types": types, "asw": a.asw, "subch": a.subch_id, "newFlag": a.new_flag, "region": a.region })
        })
        .collect();
    json!({
        "valid": true,
        "channel": channel,
        "frequency": frequency,
        "eid": eid,
        "eidHex": hex16(eid as u32),
        "ecc": ens.ecc,
        "eccHex": ens.ecc.map(|e| format!("{e:02X}")),
        "ueid": ens.ueid().map(|u| format!("{u:06X}")),
        "country": country.map(|c| c.0),
        "countryName": country.map(|c| c.1),
        "label": ens.label.as_ref().map(|l| l.text.clone()).unwrap_or_default(),
        "short": ens.label.as_ref().map(|l| l.short.clone()).unwrap_or_default(),
        "long": labels.get(&(0, eid as u32, 0)),
        "lto": ens.lto,
        "interTableId": ens.inter_table_id,
        "alarm": ens.alarm,
        "services": services,
        "subchannels": subchannels,
        "usedCu": used,
        "freeCu": 864u32.saturating_sub(used),
        "audioCu": audio_cu,
        "dataCu": data_cu,
        "frequencyInfo": fi,
        "linkage": linkage,
        "oeServices": oe,
        "announcements": announcements,
    })
}
