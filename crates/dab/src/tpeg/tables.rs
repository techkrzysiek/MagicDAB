//! English names of the TPEG2-TEC code tables (tec001 to tec009, sub-causes
//! tec101 to tec131, sub-advice tec202 to tec216) and of the application IDs.
//! Codes not in a table are named "unknown"; the raw codes go out as well.

pub(crate) const UNKNOWN: &str = "unknown";

/// TPEG application (AID) abbreviation.
pub(crate) fn application(aid: u16) -> &'static str {
    match aid {
        0 => "SNI",
        1 => "RTM",
        2 => "PTI",
        3 => "PKI",
        4 => "CTT",
        5 => "TEC",
        6 => "CAI",
        7 => "TFP",
        15 => "EAW",
        _ => UNKNOWN,
    }
}

/// tec001: effect on the traffic flow.
pub(crate) fn effect(code: u8) -> &'static str {
    match code {
        1 => "Traffic flow unknown",
        2 => "Free traffic flow",
        3 => "Heavy traffic",
        4 => "Slow traffic",
        5 => "Queuing traffic",
        6 => "Stationary traffic",
        7 => "No traffic flow",
        _ => UNKNOWN,
    }
}

/// tec002: main cause.
pub(crate) fn cause(code: u8) -> &'static str {
    match code {
        1 => "Traffic congestion",
        2 => "Accident",
        3 => "Roadworks",
        4 => "Narrow lanes",
        5 => "Impassability",
        6 => "Slippery road",
        7 => "Aquaplaning",
        8 => "Fire",
        9 => "Hazardous driving conditions",
        10 => "Objects on the road",
        11 => "Animals on the road",
        12 => "People on the road",
        13 => "Broken down vehicles",
        14 => "Vehicle on wrong carriageway",
        15 => "Rescue and recovery work",
        16 => "Regulatory measure",
        17 => "Extreme weather",
        18 => "Reduced visibility",
        19 => "Precipitation",
        20 => "Reckless persons",
        21 => "Overheight warning system triggered",
        22 => "Traffic regulations changed",
        23 => "Major event",
        24 => "Service not operating",
        25 => "Service not usable",
        26 => "Slow moving vehicles",
        27 => "Dangerous end of queue",
        28 => "Risk of fire",
        29 => "Time delay",
        30 => "Police checkpoint",
        31 => "Malfunctioning roadside equipment",
        100 => "Test message",
        _ => UNKNOWN,
    }
}

/// tec101 to tec131: sub-cause, by main cause.
pub(crate) fn sub_cause(cause: u8, code: u8) -> &'static str {
    let names: &[&str] = match cause {
        1 => &["Increased volume of traffic"],
        2 => &[
            "Multi-vehicle accident",
            "Major accident",
            "Accident involving lorry",
            "Accident involving bus",
            "Accident involving hazardous materials",
            "Accident in opposite lane",
            "Unsecured accident",
        ],
        3 => &["Major roadworks", "Road marking work", "Slow moving road maintenance"],
        4 => &["Contraflow", "Hard shoulder closed", "Slip lane closed", "Crawler lane closed"],
        5 => &["Flooding", "Danger of avalanches", "Blasting of avalanches", "Landslips", "Chemical spillage", "Winter closure"],
        6 => &[
            "Heavy frost on road",
            "Fuel on road",
            "Mud on road",
            "Snow on road",
            "Ice on road",
            "Black ice on road",
            "Oil on road",
            "Loose chippings",
            "Instant black ice",
            "Roads salted",
        ],
        8 => &["Major fire", "Forest fire"],
        9 => &[
            "Rockfalls",
            "Earthquake damage",
            "Sewer collapse",
            "Subsidence",
            "Snow drifts",
            "Storm damage",
            "Burst pipe",
            "Volcano eruption",
            "Falling ice",
        ],
        10 => &["Shed load", "Parts of vehicles", "Parts of tyres", "Large objects", "Fallen trees", "Hub caps", "Stationary vehicle"],
        11 => &["Wild animals", "Herd of animals", "Small animals", "Large animals"],
        12 => &["Children on the road", "Cyclists on the road", "Moped riders on the road"],
        13 => &["Broken down vehicle on fire", "Broken down unlit vehicle"],
        15 => &["Emergency vehicles", "Rescue helicopter landing", "Police activity", "Medical emergency", "Child abduction in progress"],
        16 => &[
            "Security alert",
            "Contagious disease",
            "Environmental",
            "Smog alert",
            "Batch service in progress",
            "Road closed by the authorities",
        ],
        17 => &["Strong winds", "Damaging hail", "Hurricane", "Thunderstorm", "Tornado", "Blizzard"],
        18 => &["Fog", "Smoke", "Heavy snowfall", "Heavy rain", "Heavy hail", "Low sun glare", "Sandstorms", "Swarms of insects"],
        19 => &["Heavy rain", "Heavy snowfall", "Soft hail"],
        20 => &["Reckless driver", "Gunfire on road", "Persons throwing objects"],
        23 => &[
            "Sports event",
            "Demonstration",
            "Demonstration with vehicles",
            "Concert",
            "Fair",
            "Military training",
            "Emergency training",
            "Festival",
            "Procession",
        ],
        24 => &["Ferry service not operating", "Air service not operating", "Train service not operating", "Bus service not operating"],
        25 => &["Fuel station closed", "Service area closed", "Service area busy", "Parking full", "Car park closed"],
        26 => &[
            "Slow moving maintenance vehicle",
            "Vehicles slowing to look at accident",
            "Abnormal load",
            "Abnormal wide load",
            "Convoy",
            "Snowplough",
            "De-icing",
            "Salting vehicles",
        ],
        27 => &["Sudden end of queue", "Queue over hill", "Queue around bend", "Queue in tunnel"],
        28 => &["Leakage of fuel", "Leakage of gas"],
        29 => &["Time delay at frontier", "Time delay at ferry port", "Time delay at vehicle-on-rail terminal"],
        30 => &["Permanent police checkpoint", "Temporary police checkpoint"],
        31 => &[
            "Road-rail crossing failure",
            "Tunnel ventilation not working",
            "Traffic signals working incorrectly",
            "Emergency telephones not working",
            "Automatic payment lanes not working",
        ],
        _ => &[],
    };
    nth(names, code)
}

/// tec003: warning level.
pub(crate) fn warning(code: u8) -> &'static str {
    match code {
        1 => "Informative",
        2 => "Danger level 1",
        3 => "Danger level 2",
        4 => "Danger level 3",
        _ => UNKNOWN,
    }
}

/// tec004: lane restriction.
pub(crate) fn lane_restriction(code: u8) -> &'static str {
    match code {
        1 => "Lanes closed",
        2 => "Lanes open",
        3 => "Right lanes closed",
        4 => "Left lanes closed",
        5 => "Hard shoulder open",
        _ => UNKNOWN,
    }
}

/// tec005: advice.
pub(crate) fn advice(code: u8) -> &'static str {
    match code {
        1 => "Drive to next parking place",
        2 => "Overtaking not allowed",
        3 => "Driving not allowed",
        4 => "Use hard shoulder as lane",
        5 => "Wait for police patrol",
        6 => "Wait for better weather",
        7 => "Make way for vehicles from behind",
        8 => "Follow diversion",
        9 => "No diversion to recommend",
        10 => "Do not divert",
        11 => "Follow police instructions",
        12 => "Avoid the area",
        13 => "Drive carefully",
        14 => "Do not leave your vehicle",
        15 => "Switch on radio",
        16 => "Use toll lanes",
        17 => "Wait for convoy",
        _ => UNKNOWN,
    }
}

/// tec202 to tec216: sub-advice, by advice.
pub(crate) fn sub_advice(advice: u8, code: u8) -> &'static str {
    let names: &[&str] = match advice {
        2 => &["Do not use overtaking lanes", "Drive on crawler lane", "Drive on leftmost lane", "Drive on rightmost lane"],
        3 => &["Pull over and stop the vehicle"],
        7 => &["Make way for rescue vehicles", "Make way for service vehicles"],
        8 => &["Follow diversion signs"],
        13 => &["Danger on entry slip road", "Danger on exit slip road", "Ice on cable structure"],
        14 => &["Do not leave your vehicle", "Do not leave your vehicle, close windows"],
        16 => &["Use manual payment toll lanes", "Use automatic payment toll lanes"],
        _ => &[],
    };
    nth(names, code)
}

/// tec006: tendency of the traffic flow speed.
pub(crate) fn tendency(code: u8) -> &'static str {
    match code {
        1 => "Slightly increasing",
        2 => "Increasing",
        3 => "Strongly increasing",
        4 => "Slightly decreasing",
        5 => "Decreasing",
        6 => "Strongly decreasing",
        7 => "Constant",
        _ => UNKNOWN,
    }
}

/// tec007: restriction type.
pub(crate) fn restriction(code: u8) -> &'static str {
    match code {
        1 => "Width less than",
        2 => "Width greater than",
        3 => "Height less than",
        4 => "Height greater than",
        5 => "Weight less than",
        6 => "Weight greater than",
        7 => "Without winter tyres",
        8 => "Without snow chains",
        9 => "With trailer",
        10 => "With caravan",
        11 => "Persons in vehicle less than",
        12 => "Persons in vehicle more than",
        13 => "Even number plate",
        14 => "Odd number plate",
        15 => "Length less than",
        16 => "Length greater than",
        17 => "Axle load less than",
        18 => "Axle load greater than",
        19 => "Emission standard EURO 3",
        20 => "Emission standard EURO 3D4",
        21 => "Emission standard EURO 4",
        22 => "Emission standard EURO 5",
        23 => "With petrol engine",
        24 => "With diesel engine",
        25 => "With LPG engine",
        26 => "Through traffic",
        27 => "Residents traffic",
        28 => "Destination in given area",
        _ => UNKNOWN,
    }
}

/// tec009: vehicle type.
pub(crate) fn vehicle(code: u8) -> &'static str {
    match code {
        1 => "Car",
        2 => "Lorry",
        3 => "Bus",
        4 => "Taxi",
        5 => "Train",
        6 => "Motorcycle",
        7 => "Vehicle with trailer",
        8 => "Motor vehicle",
        9 => "Hazardous goods vehicle",
        10 => "Abnormal size load",
        11 => "Heavy goods vehicle",
        _ => UNKNOWN,
    }
}

/// Code `n` of a table whose codes run from 1.
fn nth(names: &[&'static str], code: u8) -> &'static str {
    (code as usize).checked_sub(1).and_then(|i| names.get(i)).copied().unwrap_or(UNKNOWN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(effect(5), "Queuing traffic");
        assert_eq!(cause(3), "Roadworks");
        assert_eq!(cause(99), UNKNOWN);
        assert_eq!(sub_cause(2, 7), "Unsecured accident");
        assert_eq!(sub_cause(2, 0), UNKNOWN);
        assert_eq!(sub_cause(2, 8), UNKNOWN);
        assert_eq!(sub_cause(7, 1), UNKNOWN);
        assert_eq!(sub_advice(8, 1), "Follow diversion signs");
        assert_eq!(application(7), "TFP");
    }
}
