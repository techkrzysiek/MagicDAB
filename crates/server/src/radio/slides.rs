//! The last slide of each service, kept on disk for the clients that come later.

use super::*;

impl Radio {
    /// Key of a service's last slide on the current multiplex ("5B_3EB7_00003656").
    pub(super) fn slide_key(&self, sid: u32) -> Option<String> {
        Some(slide_key(self.channel.as_deref()?, self.ensemble.eid?, sid))
    }

    pub(super) fn store_slide(&mut self, sid: u32, header: &Value, data: &Arc<Vec<u8>>) {
        let Some(key) = self.slide_key(sid) else { return };
        self.store_slide_key(key, header, data);
    }

    /// Keep the last slide of a service on a multiplex (see `slide_key`).
    pub(super) fn store_slide_key(&mut self, key: String, header: &Value, data: &Arc<Vec<u8>>) {
        let dir = self.data_dir.join("slides");
        let _ = std::fs::create_dir_all(&dir);
        let ext = if header["mime"].as_str() == Some("image/png") { "png" } else { "jpg" };
        for e in ["png", "jpg"] {
            let _ = std::fs::remove_file(dir.join(format!("{key}.{e}")));
        }
        let _ = std::fs::write(dir.join(format!("{key}.{ext}")), data.as_slice());
        let _ = std::fs::write(dir.join(format!("{key}.json")), header.to_string());
        self.slide_cache.insert(key, (header.clone(), data.clone()));
    }

    /// The last slide of the multiplex to the listeners of its services once its
    /// ensemble is known after tuning (unless one is being received).
    pub(super) fn send_cached_slides(&self) {
        for id in self.hub.ids_in(0) {
            let Some(sid) = self.hub.session(id).and_then(|s| s.3) else { continue };
            let live = self.services.iter().any(|(k, st)| k.sid == sid && st.slide.is_some());
            if let (false, Some((h, data))) = (live, self.slide_key(sid).and_then(|k| self.slide_cache.get(&k))) {
                self.hub.send(id, binary(BIN_IMAGE, h, data));
            }
        }
    }
}

/// Key of the last slide of a service on a multiplex ("5B_3EB7_00003656").
pub(super) fn slide_key(channel: &str, eid: u16, sid: u32) -> String {
    format!("{channel}_{eid:04X}_{sid:08X}")
}

/// Category and slide id of a categorised slideshow (category 0: none).
pub(super) fn slide_category(object: &magicdab_dab::mot::MotObject) -> Option<(u8, u8)> {
    object.param(0x25).filter(|d| d.len() >= 2).map(|d| (d[0], d[1]))
}

/// Header of a slide for the interface (`catslide`: its category, if any).
pub(super) fn slide_header(key: ServiceKey, object: &magicdab_dab::mot::MotObject, catslide: Option<(u8, u8)>) -> Value {
    let cat_title = object.param(0x26).map(|d| String::from_utf8_lossy(d).to_string());
    let click = object.param(0x27).map(|d| String::from_utf8_lossy(d).to_string());
    json!({
        "kind": "slide",
        "sid": key.sid,
        "scids": key.scids,
        "contentName": object.content_name,
        "mime": object.mime_type(),
        "size": object.body.len(),
        "transportId": object.transport_id,
        "categoryTitle": cat_title,
        "clickThroughUrl": click,
        "category": catslide.map(|c| c.0),
        "slideId": catslide.map(|c| c.1),
        "received": now(),
    })
}

pub(super) fn load_slide_cache(dir: &std::path::Path) -> HashMap<String, (Value, Arc<Vec<u8>>)> {
    let mut m = HashMap::new();
    let dir = dir.join("slides");
    let Ok(rd) = std::fs::read_dir(&dir) else { return m };
    for e in rd.flatten() {
        let p = e.path();
        let Some(stem) = p.file_stem().and_then(|x| x.to_str()).map(|s| s.to_string()) else { continue };
        if !stem.contains('_') {
            // slides of the service on any multiplex (before per multiplex keys)
            let _ = std::fs::remove_file(&p);
            continue;
        }
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Some(h) = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) else { continue };
        let img = ["jpg", "png"].iter().find_map(|x| std::fs::read(dir.join(format!("{stem}.{x}"))).ok());
        if let Some(img) = img.filter(|d| crate::spi::sniff_mime(d) != "application/octet-stream") {
            m.insert(stem, (h, Arc::new(img)));
        }
    }
    m
}
