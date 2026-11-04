//! Local time without panics: chrono's `Local::now()` panics when the system
//! cannot tell its time zone (seen under Wine); UTC is used then.

use chrono::{DateTime, FixedOffset, TimeZone, Utc};

/// Offset of the local time zone at `t`.
pub fn offset_at(t: DateTime<Utc>) -> FixedOffset {
    let utc = t.naive_utc();
    // the offset of the UTC wall time, then of the local wall time it gives
    let guess = chrono::Local.offset_from_local_datetime(&utc).earliest();
    let off = guess.and_then(|g| chrono::Local.offset_from_local_datetime(&(utc + g)).earliest()).or(guess);
    off.unwrap_or_else(|| FixedOffset::east_opt(0).unwrap())
}

pub fn now() -> DateTime<FixedOffset> {
    at(Utc::now())
}

pub fn at(t: DateTime<Utc>) -> DateTime<FixedOffset> {
    t.with_timezone(&offset_at(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_as_chrono() {
        let t = Utc::now();
        assert_eq!(at(t), t.with_timezone(&chrono::Local).fixed_offset());
    }
}
