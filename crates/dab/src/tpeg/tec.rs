//! TPEG2-TEC messages (ISO 21219-15): message management container (MMC,
//! ISO 21219-6), event container with causes and advice, location container.

use super::binary::{Component, Reader};
use super::{lrc, tables, Advice, Cause, Code, Condition, SpeedLimit, TrafficEvent, VehicleRestriction};

/// Message management container.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Mmc {
    pub id: u32,
    pub version: u8,
    pub expires: u32,
    pub cancel: bool,
    pub generated: Option<u32>,
    pub priority: Option<u8>,
}

/// Component ids of a TEC message.
const MMC: u8 = 1;
const LRC: u8 = 2;
const EVENT: u8 = 3;
const DIRECT_CAUSE: u8 = 4;
const LINKED_CAUSE: u8 = 5;
const ADVICE: u8 = 6;
const VEHICLE_RESTRICTION: u8 = 7;
const DIVERSION_ROUTE: u8 = 8;
const TEMPORARY_SPEED_LIMIT: u8 = 11;

fn mmc(c: &Component) -> Option<Mmc> {
    let mut r = c.attributes();
    let mut m = Mmc { id: r.mb()?, version: r.u8()?, expires: r.u32()?, ..Default::default() };
    let s = r.selector()?;
    m.cancel = s.has(0);
    if s.has(1) {
        m.generated = Some(r.u32()?);
    }
    if s.has(2) {
        m.priority = Some(r.u8()?);
    }
    Some(m)
}

/// Decode a TEC message (component 0). Returns its MMC and the event (the
/// key fields are left for the caller); `None` without a readable MMC.
pub(crate) fn decode(msg: &Component) -> Option<(Mmc, TrafficEvent)> {
    if msg.id != 0 || !msg.well_formed() {
        return None;
    }
    let m = msg.children().find(|c| c.id == MMC).and_then(|c| mmc(&c))?;
    let mut ev = TrafficEvent {
        application: "TEC",
        id: m.id,
        version: m.version,
        expires: m.expires as u64,
        generated: m.generated.map(u64::from),
        priority: m.priority,
        ..Default::default()
    };
    for c in msg.children() {
        match c.id {
            EVENT => event(&c, &mut ev),
            LRC => ev.location = Some(lrc::decode(&c)),
            _ => {}
        }
    }
    ev.warning = ev.causes.iter().filter_map(|c| c.warning).max_by_key(|w| w.code);
    Some((m, ev))
}

fn code(code: u8, name: fn(u8) -> &'static str) -> Code {
    Code { code, name: name(code) }
}

fn event(c: &Component, ev: &mut TrafficEvent) {
    let mut r = c.attributes();
    // attributes after a malformed or unknown one are left out
    let _ = (|| {
        ev.effect = Some(code(r.u8()?, tables::effect));
        let s = r.selector()?;
        if s.has(0) {
            ev.start = Some(r.u32()? as u64);
        }
        if s.has(1) {
            ev.stop = Some(r.u32()? as u64);
        }
        if s.has(2) {
            ev.tendency = Some(code(r.u8()?, tables::tendency));
        }
        if s.has(3) {
            ev.length_affected = Some(r.mb()?);
        }
        if s.has(4) {
            ev.average_speed = Some(r.u8()?);
        }
        if s.has(5) {
            ev.delay = Some(r.mb()?);
        }
        if s.has(6) {
            ev.speed_limit = Some(r.u8()?);
        }
        if s.has(7) {
            ev.expected_speed = Some(r.u8()?);
        }
        Some(())
    })();
    for sub in c.children() {
        match sub.id {
            DIRECT_CAUSE => ev.causes.extend(direct_cause(&sub)),
            LINKED_CAUSE => ev.causes.extend(linked_cause(&sub)),
            ADVICE => ev.advice.extend(advice(&sub)),
            VEHICLE_RESTRICTION => ev.restrictions.extend(vehicle_restriction(&sub)),
            DIVERSION_ROUTE => ev.diversion_routes += 1,
            TEMPORARY_SPEED_LIMIT => ev.speed_limits.extend(speed_limits(&sub)),
            _ => {}
        }
    }
}

fn new_cause(main: u8) -> Cause {
    Cause {
        code: main,
        name: tables::cause(main),
        sub_code: None,
        sub_name: None,
        warning: None,
        unverified: false,
        length_affected: None,
        lane_restriction: None,
        lanes: None,
        text: Vec::new(),
        offset: None,
        linked_message: None,
    }
}

fn direct_cause(c: &Component) -> Option<Cause> {
    let mut r = c.attributes();
    let mut cause = new_cause(r.u8()?);
    cause.warning = Some(code(r.u8()?, tables::warning));
    let _ = (|| {
        let s = r.selector()?;
        cause.unverified = s.has(0);
        if s.has(1) {
            let sub = r.u8()?;
            cause.sub_code = Some(sub);
            cause.sub_name = Some(tables::sub_cause(cause.code, sub));
        }
        if s.has(2) {
            cause.length_affected = Some(r.mb()?);
        }
        if s.has(3) {
            cause.lane_restriction = Some(code(r.u8()?, tables::lane_restriction));
        }
        if s.has(4) {
            cause.lanes = Some(r.u8()?);
        }
        if s.has(5) {
            for _ in 0..r.mb()? {
                cause.text.push(r.localised_short_string()?);
            }
        }
        if s.has(6) {
            cause.offset = Some(r.mb()?);
        }
        Some(())
    })();
    Some(cause)
}

/// A cause given by another message: main cause and that message's ID.
fn linked_cause(c: &Component) -> Option<Cause> {
    let mut r = c.attributes();
    let mut cause = new_cause(r.u8()?);
    cause.linked_message = Some(r.mb()?);
    Some(cause)
}

fn localised_texts(r: &mut Reader) -> Option<Vec<String>> {
    (0..r.mb()?).map(|_| r.localised_short_string()).collect()
}

fn advice(c: &Component) -> Option<Advice> {
    let mut r = c.attributes();
    let s = r.selector()?;
    let mut a = Advice::default();
    let _ = (|| {
        if s.has(0) {
            a.code = Some(r.u8()?);
            a.name = a.code.map(tables::advice);
        }
        if s.has(1) {
            let sub = r.u8()?;
            a.sub_code = Some(sub);
            a.sub_name = Some(tables::sub_advice(a.code.unwrap_or(0), sub));
        }
        if s.has(2) {
            a.text = localised_texts(&mut r)?;
        }
        Some(())
    })();
    Some(a)
}

fn vehicle_restriction(c: &Component) -> Option<VehicleRestriction> {
    let mut r = c.attributes();
    let s = r.selector()?;
    let mut v = VehicleRestriction::default();
    let _ = (|| {
        if s.has(0) {
            v.vehicle = Some(code(r.u8()?, tables::vehicle));
        }
        if s.has(1) {
            for _ in 0..r.mb()? {
                let kind = r.u8()?;
                let mut cond = Condition { code: kind, name: tables::restriction(kind), value: None };
                let t = r.selector()?;
                if t.has(0) {
                    cond.value = Some(r.mb()?);
                }
                v.conditions.push(cond);
                if t.has(1) {
                    // a location of the restriction: not decoded, skipped
                    r.component()?;
                }
            }
        }
        Some(())
    })();
    Some(v)
}

fn speed_limits(c: &Component) -> Vec<SpeedLimit> {
    let mut r = c.attributes();
    let mut out = Vec::new();
    let _ = (|| {
        for _ in 0..r.mb()? {
            let mut l = SpeedLimit { value: r.u8()?, ..Default::default() };
            let s = r.selector()?;
            if s.has(0) {
                l.wet = Some(r.u8()?);
            }
            if s.has(1) {
                l.length = Some(r.mb()?);
            }
            out.push(l);
        }
        let mph = r.selector()?.has(0);
        for l in out.iter_mut() {
            l.mph = mph;
        }
        Some(())
    })();
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::binary::build::{component, mb, selector};
    use super::super::lrc::tests::{near, olr_line, tmc_location};
    use super::*;

    pub fn mmc_bytes(id: u32, version: u8, expires: u32, cancel: bool) -> Vec<u8> {
        let mut a = mb(id);
        a.push(version);
        a.extend(expires.to_be_bytes());
        if cancel {
            a.extend(selector(&[0]));
        } else {
            a.extend(selector(&[2]));
            a.push(1);
        }
        component(MMC, &a, &[])
    }

    /// A TEC message: queuing traffic caused by roadworks (danger level 1,
    /// sub-cause major roadworks, one lane closed) and an accident, advice to
    /// follow a diversion, on a line location with a TMC code.
    pub fn tec_message(id: u32, version: u8, expires: u32) -> Vec<u8> {
        let mut ev = vec![5];
        ev.extend(selector(&[0, 1, 3, 5]));
        ev.extend(1_000_000u32.to_be_bytes());
        ev.extend(1_000_600u32.to_be_bytes());
        ev.extend(mb(2500));
        ev.extend(mb(15));
        let mut cause = vec![3, 2];
        cause.extend(selector(&[1, 3, 4, 5]));
        cause.extend([1, 1, 1]);
        cause.extend(mb(1));
        cause.extend([9, 4]);
        cause.extend(b"text");
        let accident = [2, 3, 0];
        let advice = [selector(&[0, 1]), vec![8, 1]].concat();
        let restriction = [selector(&[0, 1]), vec![2, 1, 6], selector(&[0]), mb(7500)].concat();
        let event = component(
            EVENT,
            &ev,
            &[
                &component(DIRECT_CAUSE, &cause, &[]),
                &component(DIRECT_CAUSE, &accident, &[]),
                &component(ADVICE, &advice, &[]),
                &component(VEHICLE_RESTRICTION, &restriction, &[]),
                &component(DIVERSION_ROUTE, &[0], &[]),
            ],
        );
        let lrc = component(LRC, &[], &[&olr_line(), &tmc_location()]);
        component(0, &[], &[&mmc_bytes(id, version, expires, false), &event, &lrc])
    }

    fn decode_bytes(b: &[u8]) -> Option<(Mmc, TrafficEvent)> {
        decode(&Reader::new(b).component()?)
    }

    #[test]
    fn full_message() {
        let (m, ev) = decode_bytes(&tec_message(4321, 7, 2_000_000)).unwrap();
        assert_eq!(m, Mmc { id: 4321, version: 7, expires: 2_000_000, cancel: false, generated: None, priority: Some(1) });
        assert_eq!(ev.effect, Some(Code { code: 5, name: "Queuing traffic" }));
        assert_eq!((ev.start, ev.stop, ev.length_affected, ev.delay), (Some(1_000_000), Some(1_000_600), Some(2500), Some(15)));
        assert_eq!(ev.causes.len(), 2);
        let c = &ev.causes[0];
        assert_eq!((c.code, c.name, c.sub_code, c.sub_name), (3, "Roadworks", Some(1), Some("Major roadworks")));
        assert_eq!(c.warning, Some(Code { code: 2, name: "Danger level 1" }));
        assert_eq!(c.lane_restriction, Some(Code { code: 1, name: "Lanes closed" }));
        assert_eq!((c.lanes, c.text.as_slice()), (Some(1), &["text".to_string()][..]));
        assert_eq!((ev.causes[1].code, ev.causes[1].warning.unwrap().code), (2, 3));
        // the highest warning level of the causes
        assert_eq!(ev.warning, Some(Code { code: 3, name: "Danger level 2" }));
        assert_eq!(ev.advice[0].name, Some("Follow diversion"));
        assert_eq!(ev.advice[0].sub_name, Some("Follow diversion signs"));
        let r = &ev.restrictions[0];
        assert_eq!(r.vehicle, Some(Code { code: 2, name: "Lorry" }));
        assert_eq!(r.conditions[0], Condition { code: 6, name: "Weight greater than", value: Some(7500) });
        assert_eq!(ev.diversion_routes, 1);
        let loc = ev.location.unwrap();
        assert!(near(&loc.points, &[[50.0, 20.0], [50.001, 20.002]]));
        assert_eq!(loc.tmc.unwrap().location, 12345);
    }

    #[test]
    fn cancellation() {
        let msg = component(0, &[], &[&mmc_bytes(99, 3, 2_000_000, true)]);
        let (m, ev) = decode_bytes(&msg).unwrap();
        assert!(m.cancel);
        assert_eq!((ev.id, ev.version, ev.effect), (99, 3, None));
    }

    #[test]
    fn unknown_attributes_and_components_are_skipped() {
        // an event with an attribute flag this decoder does not know and an
        // unknown sub-component
        let mut ev = vec![2];
        ev.extend(selector(&[9]));
        ev.extend([1, 2, 3]);
        let event = component(EVENT, &ev, &[&component(42, &[1], &[]), &component(DIRECT_CAUSE, &[1, 1, 0], &[])]);
        let msg = component(0, &[], &[&mmc_bytes(1, 0, 5, false), &event, &component(77, &[], &[])]);
        let (_, ev) = decode_bytes(&msg).unwrap();
        assert_eq!(ev.effect.unwrap().code, 2);
        assert_eq!(ev.causes[0].name, "Traffic congestion");
        assert!(ev.location.is_none());
    }

    #[test]
    fn broken_messages() {
        // no MMC
        assert!(decode_bytes(&component(0, &[], &[&component(EVENT, &[1, 0], &[])])).is_none());
        // a sub-component overrunning the message
        let msg = component(0, &[], &[&mmc_bytes(1, 0, 5, false), &[EVENT, 50, 0]]);
        assert!(decode_bytes(&msg).is_none());
        // every truncation of the attribute blocks is survived
        let full = tec_message(1, 0, 5);
        for n in 0..full.len() {
            decode_bytes(&full[..n]);
        }
    }
}
