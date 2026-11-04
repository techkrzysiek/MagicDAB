//! Slides of the services of the multiplex seen during this session (the
//! Slides view): the categorised slideshow (CatSLS, ETSI TS 101 499: category
//! and slide ids, category titles, click-through links), and for stations
//! without categories the last slides shown.

use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

/// Last slides kept per service without categories, slides per service in all,
/// and slides of all services together.
const RECENT: usize = 30;
const PER_SERVICE: usize = 64;
const TOTAL: usize = 400;

struct Slide {
    id: u64,
    cat: Option<u8>,
    slide: Option<u8>,
    header: Value,
    data: Arc<Vec<u8>>,
}

#[derive(Default)]
pub struct Gallery {
    slides: HashMap<u32, VecDeque<Slide>>,
    next_id: u64,
}

impl Gallery {
    /// Another multiplex: its slides start anew.
    pub fn clear(&mut self) {
        self.slides.clear();
    }

    /// Keep a slide shown by `sid`; `catslide` is its category and slide id
    /// (MOT parameter 0x25) when the station categorises its slides.
    pub fn add(&mut self, sid: u32, catslide: Option<(u8, u8)>, header: &Value, data: &Arc<Vec<u8>>) {
        let list = self.slides.entry(sid).or_default();
        match catslide {
            // a new version of the slide
            Some((c, s)) => list.retain(|x| !(x.cat == Some(c) && x.slide == Some(s))),
            // the same picture again
            None => list.retain(|x| !(x.cat.is_none() && *x.data == **data)),
        }
        self.next_id += 1;
        list.push_back(Slide {
            id: self.next_id,
            cat: catslide.map(|c| c.0),
            slide: catslide.map(|c| c.1),
            header: header.clone(),
            data: data.clone(),
        });
        while list.iter().filter(|x| x.cat.is_none()).count() > RECENT {
            if let Some(i) = list.iter().position(|x| x.cat.is_none()) {
                list.remove(i);
            }
        }
        while list.len() > PER_SERVICE {
            list.pop_front();
        }
        // all services together: the oldest slides go
        while self.slides.values().map(|l| l.len()).sum::<usize>() > TOTAL {
            let Some(sid) = self.slides.iter().filter(|(_, l)| !l.is_empty()).min_by_key(|(_, l)| l[0].id).map(|(&k, _)| k) else { break };
            if let Some(l) = self.slides.get_mut(&sid) {
                l.pop_front();
            }
        }
    }

    /// A slide taken out of its category (CategoryID 0 with its SlideID).
    pub fn decategorise(&mut self, sid: u32, slide: u8) {
        if let Some(list) = self.slides.get_mut(&sid) {
            list.retain(|x| x.cat.is_none() || x.slide != Some(slide));
        }
    }

    pub fn index_json(&self, sid: u32) -> Value {
        let slides: Vec<Value> = self
            .slides
            .get(&sid)
            .map(|l| {
                l.iter()
                    .map(|x| {
                        json!({
                            "id": x.id,
                            "category": x.cat,
                            "slide": x.slide,
                            "categoryTitle": x.header["categoryTitle"],
                            "clickThroughUrl": x.header["clickThroughUrl"],
                            "contentName": x.header["contentName"],
                            "mime": x.header["mime"],
                            "received": x.header["received"],
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        json!({"type": "gallery", "sid": sid, "slides": slides})
    }

    /// Pictures of `sid` the client does not have yet: (header, data).
    pub fn images(&self, sid: u32, have: &[u64]) -> Vec<(Value, Arc<Vec<u8>>)> {
        self.slides
            .get(&sid)
            .map(|l| {
                l.iter()
                    .filter(|x| !have.contains(&x.id))
                    .map(|x| (json!({"kind": "gallery", "sid": sid, "id": x.id, "mime": x.header["mime"]}), x.data.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pic(n: u8) -> Arc<Vec<u8>> {
        Arc::new(vec![n; 10])
    }

    #[test]
    fn categories_keep_the_newest_version() {
        let mut g = Gallery::default();
        let h = json!({"mime": "image/jpeg"});
        g.add(1, Some((1, 1)), &h, &pic(1));
        g.add(1, Some((1, 2)), &h, &pic(2));
        g.add(1, Some((1, 1)), &h, &pic(3));
        let idx = g.index_json(1);
        assert_eq!(idx["slides"].as_array().unwrap().len(), 2);
        assert_eq!(g.images(1, &[]).len(), 2);
        // pictures already shown are not sent again
        let have: Vec<u64> = idx["slides"].as_array().unwrap().iter().map(|s| s["id"].as_u64().unwrap()).collect();
        assert!(g.images(1, &have).is_empty());
    }

    #[test]
    fn recent_slides_without_repetitions() {
        let mut g = Gallery::default();
        let h = json!({});
        for i in 0..40u8 {
            g.add(2, None, &h, &pic(i % 35));
        }
        assert_eq!(g.index_json(2)["slides"].as_array().unwrap().len(), RECENT);
    }
}
