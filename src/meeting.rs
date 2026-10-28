//! Turns iCalendar invitations, responses and cancellations (text/calendar
//! parts, RFC 5546 iTIP) into Outlook meeting objects (MS-OXOCAL).

use std::collections::HashMap;

use crate::eml::unix_to_filetime;
use crate::ical::{
    self, Component, Property, Time, TimeZone, civil_from_days, parse_duration, parse_rrule,
    parse_time,
};
use crate::ltp::PropValue;
use crate::messaging::{Attachment, Message, PropKey};
use crate::named_props::{Guid, PSETID_APPOINTMENT, PSETID_COMMON, PSETID_MEETING, PropName};
use crate::recurrence::{self, Exception, Pattern, WindowsZone};

pub const CLASS_REQUEST: &str = "IPM.Schedule.Meeting.Request";
pub const CLASS_CANCELED: &str = "IPM.Schedule.Meeting.Canceled";
pub const CLASS_RESP_POS: &str = "IPM.Schedule.Meeting.Resp.Pos";
pub const CLASS_RESP_NEG: &str = "IPM.Schedule.Meeting.Resp.Neg";
pub const CLASS_RESP_TENT: &str = "IPM.Schedule.Meeting.Resp.Tent";

/// Class ID that starts every PidLidGlobalObjectId (MS-OXOCAL 2.2.1.27).
const GOID_CLASS_ID: [u8; 16] = [
    0x04, 0x00, 0x00, 0x00, 0x82, 0x00, 0xE0, 0x00, 0x74, 0xC5, 0xB7, 0x10, 0x1A, 0x82, 0xE0, 0x08,
];

// PSETID_Appointment
const LID_SEQUENCE: u32 = 0x8201;
const LID_BUSY_STATUS: u32 = 0x8205;
const LID_LOCATION: u32 = 0x8208;
const LID_START_WHOLE: u32 = 0x820D;
const LID_END_WHOLE: u32 = 0x820E;
const LID_DURATION: u32 = 0x8213;
const LID_SUB_TYPE: u32 = 0x8215;
const LID_RECUR: u32 = 0x8216;
const LID_STATE_FLAGS: u32 = 0x8217;
const LID_RESPONSE_STATUS: u32 = 0x8218;
const LID_RECURRING: u32 = 0x8223;
const LID_INTENDED_BUSY_STATUS: u32 = 0x8224;
const LID_FINVITED: u32 = 0x8229;
const LID_RECURRENCE_PATTERN: u32 = 0x8232;
const LID_TIME_ZONE_STRUCT: u32 = 0x8233;
const LID_TIME_ZONE_DESCRIPTION: u32 = 0x8234;
const LID_CLIP_START: u32 = 0x8235;
const LID_CLIP_END: u32 = 0x8236;
const LID_ALL_ATTENDEES: u32 = 0x8238;
const LID_TO_ATTENDEES: u32 = 0x823B;
const LID_CC_ATTENDEES: u32 = 0x823C;
const LID_PROPOSED_START: u32 = 0x8250;
const LID_PROPOSED_END: u32 = 0x8251;
const LID_COUNTER_PROPOSAL: u32 = 0x8257;
const LID_TZDEF_START_DISPLAY: u32 = 0x825E;
const LID_TZDEF_END_DISPLAY: u32 = 0x825F;
const LID_TZDEF_RECUR: u32 = 0x8260;
// PSETID_Meeting
const LID_ATTENDEE_CRITICAL_CHANGE: u32 = 0x0001;
const LID_WHERE: u32 = 0x0002;
const LID_GLOBAL_OBJECT_ID: u32 = 0x0003;
const LID_IS_RECURRING: u32 = 0x0005;
const LID_IS_EXCEPTION: u32 = 0x000A;
const LID_OWNER_CRITICAL_CHANGE: u32 = 0x001A;
const LID_CLEAN_GLOBAL_OBJECT_ID: u32 = 0x0023;
const LID_MEETING_TYPE: u32 = 0x0026;
// PSETID_Common
const LID_REMINDER_DELTA: u32 = 0x8501;
const LID_REMINDER_TIME: u32 = 0x8502;
const LID_REMINDER_SET: u32 = 0x8503;
const LID_SIDE_EFFECTS: u32 = 0x8510;
const LID_COMMON_START: u32 = 0x8516;
const LID_COMMON_END: u32 = 0x8517;
const LID_REMINDER_SIGNAL_TIME: u32 = 0x8560;

const PR_SENSITIVITY: u16 = 0x0036;
const PR_SUBJECT_PREFIX: u16 = 0x003D;
const PR_CONVERSATION_TOPIC: u16 = 0x0070;
const PR_NORMALIZED_SUBJECT: u16 = 0x0E1D;
const PR_START_DATE: u16 = 0x0060;
const PR_END_DATE: u16 = 0x0061;
const PR_RESPONSE_REQUESTED: u16 = 0x0063;

const BUSY_FREE: i32 = 0;
const BUSY_TENTATIVE: i32 = 1;
const BUSY_BUSY: i32 = 2;
const BUSY_OOF: i32 = 3;
const BUSY_WORKING_ELSEWHERE: i32 = 4;
const ASF_MEETING: i32 = 0x1;
const ASF_RECEIVED: i32 = 0x2;
const ASF_CANCELED: i32 = 0x4;
const RESP_NONE: i32 = 0;
const RESP_NOT_RESPONDED: i32 = 5;
const MTG_REQUEST: i32 = 0x0000_0001;
const MTG_FULL_UPDATE: i32 = 0x0001_0000;
/// Side effects Outlook sets on meeting objects (open-to-delete/copy/move,
/// coerce to inbox, context menu actions).
const SIDE_EFFECTS_MEETING: i32 = 0x1C61;
/// PidLidClipEnd of a series without an end: 4500-08-31 23:59.
const CLIP_END_NEVER: i64 = 0x0CB3_4557_A3DD_4000;

fn is_calendar(att: &Attachment) -> bool {
    matches!(
        att.mime_type.as_deref(),
        Some("text/calendar" | "application/ics")
    ) || att
        .filename
        .as_deref()
        .is_some_and(|f| f.to_ascii_lowercase().ends_with(".ics"))
}

struct Props(Vec<(PropKey, PropValue)>);

impl Props {
    fn named(&mut self, guid: Guid, lid: u32, value: PropValue) {
        self.0
            .push((PropKey::Named(guid, PropName::Id(lid)), value));
    }

    fn appt(&mut self, lid: u32, value: PropValue) {
        self.named(PSETID_APPOINTMENT, lid, value);
    }

    fn meeting(&mut self, lid: u32, value: PropValue) {
        self.named(PSETID_MEETING, lid, value);
    }

    fn common(&mut self, lid: u32, value: PropValue) {
        self.named(PSETID_COMMON, lid, value);
    }

    fn tag(&mut self, id: u16, value: PropValue) {
        self.0.push((PropKey::Id(id), value));
    }
}

/// A resolved DTSTART/DTEND: the absolute time and the local wall time in the
/// event's time zone (UTC for UTC and floating times).
#[derive(Clone, Copy, Debug)]
struct When {
    utc: i64,
    wall: i64,
    all_day: bool,
}

struct Zones(HashMap<String, TimeZone>);

impl Zones {
    fn zone(&self, prop: &Property) -> Option<&TimeZone> {
        self.0.get(prop.param("TZID")?.trim_matches('"'))
    }

    fn resolve(&self, prop: &Property) -> Option<When> {
        let value = prop.value.split(',').next()?;
        Some(match parse_time(value)? {
            Time::Utc(t) => When {
                utc: t,
                wall: self.zone(prop).map_or(t, |z| z.utc_to_wall(t)),
                all_day: false,
            },
            Time::Wall(t) => When {
                utc: self.zone(prop).map_or(t, |z| z.wall_to_utc(t)),
                wall: t,
                all_day: false,
            },
            Time::Date(d) => When {
                utc: d * 86_400,
                wall: d * 86_400,
                all_day: true,
            },
        })
    }
}

/// PidLidGlobalObjectId for an iCalendar UID (MS-OXOCAL 2.2.1.27). UIDs that
/// Exchange derived from a global object ID are decoded back to it; others are
/// wrapped as "vCal-Uid" data, as Exchange and Outlook do.
fn global_object_id(uid: &str, instance_day: Option<i64>) -> Vec<u8> {
    let decoded = (uid.len().is_multiple_of(2)
        && uid.len() >= 80
        && uid
            .to_ascii_uppercase()
            .starts_with("040000008200E00074C5B7101A82E008"))
    .then(|| {
        (0..uid.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&uid[i..i + 2], 16))
            .collect::<Result<Vec<u8>, _>>()
            .ok()
    })
    .flatten();
    let mut goid = decoded.unwrap_or_else(|| {
        let mut data = b"vCal-Uid".to_vec();
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(uid.as_bytes());
        data.push(0);
        let mut g = GOID_CLASS_ID.to_vec();
        g.extend_from_slice(&[0u8; 4 + 8 + 8]); // instance date, creation time, reserved
        g.extend_from_slice(&(data.len() as u32).to_le_bytes());
        g.extend(data);
        g
    });
    let instance = instance_day.map_or([0u8; 4], |day| {
        let (y, m, d) = civil_from_days(day);
        [(y >> 8) as u8, y as u8, m as u8, d as u8]
    });
    goid[16..20].copy_from_slice(&instance);
    goid
}

/// Free/busy status from an X-MICROSOFT-CDO status property, or from TRANSP.
fn busy_status(ev: &Component, property: &str) -> i32 {
    match ev
        .value(property)
        .map(|s| s.trim().to_ascii_uppercase())
        .as_deref()
    {
        Some("FREE") => BUSY_FREE,
        Some("TENTATIVE") => BUSY_TENTATIVE,
        Some("OOF") => BUSY_OOF,
        Some("WORKINGELSEWHERE") => BUSY_WORKING_ELSEWHERE,
        Some("BUSY") => BUSY_BUSY,
        _ if ev
            .value("TRANSP")
            .is_some_and(|t| t.trim().eq_ignore_ascii_case("TRANSPARENT")) =>
        {
            BUSY_FREE
        }
        _ => BUSY_BUSY,
    }
}

fn attendee_name(p: &Property) -> String {
    p.param("CN")
        .map(|n| n.trim_matches('"').to_owned())
        .unwrap_or_else(|| {
            p.value
                .trim()
                .trim_start_matches("mailto:")
                .trim_start_matches("MAILTO:")
                .to_owned()
        })
}

/// Plain English description of a recurrence, as in PidLidRecurrencePattern.
fn describe(rule: &ical::RRule, start_minute: u32, end_minute: u32) -> String {
    const DAYS: [&str; 7] = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    let unit = match rule.freq {
        ical::Freq::Daily => "day",
        ical::Freq::Weekly => "week",
        ical::Freq::Monthly => "month",
        ical::Freq::Yearly => "year",
    };
    let every = if rule.interval == 1 {
        format!("every {unit}")
    } else {
        format!("every {} {unit}s", rule.interval)
    };
    let on = if rule.by_day.is_empty() {
        String::new()
    } else {
        let days: Vec<&str> = rule.by_day.iter().map(|(_, d)| DAYS[*d as usize]).collect();
        format!(" on {}", days.join(", "))
    };
    let hm = |m: u32| format!("{:02}:{:02}", m / 60 % 24, m % 60);
    format!(
        "Occurs {every}{on} from {} to {}",
        hm(start_minute),
        hm(end_minute)
    )
}

/// Converts the first iCalendar part of `msg` into meeting properties. Returns
/// false, leaving the message unchanged, when the part is not a meeting
/// request, response or cancellation.
pub fn apply(msg: &mut Message) -> bool {
    let Some(index) = msg.attachments.iter().position(is_calendar) else {
        return false;
    };
    let text = String::from_utf8_lossy(&msg.attachments[index].data).into_owned();
    let Some(cal) = ical::parse(&text) else {
        return false;
    };
    let zones = Zones(
        cal.children("VTIMEZONE")
            .filter_map(TimeZone::from_component)
            .map(|z| (z.id.clone(), z))
            .collect(),
    );
    let mut events: Vec<&Component> = cal.children("VEVENT").collect();
    events.sort_by_key(|e| e.prop("RECURRENCE-ID").is_some());
    let Some(ev) = events.first().copied() else {
        return false;
    };
    let method = cal
        .value("METHOD")
        .unwrap_or_default()
        .trim()
        .to_ascii_uppercase();

    let attendees: Vec<&Property> = ev.props("ATTENDEE").collect();
    let partstat = attendees
        .first()
        .and_then(|a| a.param("PARTSTAT"))
        .map(|s| s.to_ascii_uppercase());
    let class = match (method.as_str(), partstat.as_deref()) {
        ("REQUEST" | "ADD", _) => CLASS_REQUEST,
        ("CANCEL", _) => CLASS_CANCELED,
        ("REPLY", Some("ACCEPTED")) => CLASS_RESP_POS,
        ("REPLY", Some("DECLINED")) => CLASS_RESP_NEG,
        ("REPLY", Some("TENTATIVE")) | ("COUNTER", _) => CLASS_RESP_TENT,
        _ => return false,
    };
    let canceled = class == CLASS_CANCELED;
    let Some(start) = ev.prop("DTSTART").and_then(|p| zones.resolve(p)) else {
        return false;
    };
    let end = ev
        .prop("DTEND")
        .and_then(|p| zones.resolve(p))
        .or_else(|| {
            let secs = ev.value("DURATION").and_then(parse_duration)?;
            Some(When {
                utc: start.utc + secs,
                wall: start.wall + secs,
                all_day: start.all_day,
            })
        })
        .unwrap_or(When {
            utc: start.utc + if start.all_day { 86_400 } else { 0 },
            wall: start.wall + if start.all_day { 86_400 } else { 0 },
            all_day: start.all_day,
        });
    let tz = ev.prop("DTSTART").and_then(|p| zones.zone(p));
    let year = civil_from_days(start.wall.div_euclid(86_400)).0;
    let wzone = tz.map_or(WindowsZone::UTC, |z| WindowsZone::from_tz(z, year));
    let tz_name = tz.map(|z| z.id.clone()).unwrap_or_else(|| "UTC".to_owned());
    let ft = |t: i64| PropValue::Time(unix_to_filetime(t));

    let mut p = Props(Vec::new());
    let uid = ev.text("UID").unwrap_or_default();
    let recurrence_id = ev.prop("RECURRENCE-ID").and_then(|r| zones.resolve(r));
    let instance_day = recurrence_id.map(|r| r.wall.div_euclid(86_400));
    p.meeting(
        LID_GLOBAL_OBJECT_ID,
        PropValue::Binary(global_object_id(&uid, instance_day)),
    );
    p.meeting(
        LID_CLEAN_GLOBAL_OBJECT_ID,
        PropValue::Binary(global_object_id(&uid, None)),
    );
    let sequence: i32 = ev
        .value("SEQUENCE")
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    p.appt(LID_SEQUENCE, PropValue::Integer32(sequence));
    if let Some(stamp) = ev.value("DTSTAMP").and_then(parse_time) {
        let t = match stamp {
            Time::Utc(t) | Time::Wall(t) => t,
            Time::Date(d) => d * 86_400,
        };
        p.meeting(LID_OWNER_CRITICAL_CHANGE, ft(t));
        p.meeting(LID_ATTENDEE_CRITICAL_CHANGE, ft(t));
    }
    let location = ev.text("LOCATION").unwrap_or_default();
    p.appt(LID_LOCATION, PropValue::Unicode(location.clone()));
    p.meeting(LID_WHERE, PropValue::Unicode(location));

    // A recurring series: the pattern, time zone and first occurrence.
    let rule = ev.value("RRULE").and_then(parse_rrule);
    let start_day = start.wall.div_euclid(86_400);
    // A rule Outlook has no pattern for is, when bounded, written as the
    // exact set of its occurrences: a daily pattern with the other days deleted.
    let (pattern, covered_gaps) = match rule.as_ref() {
        Some(r) => match Pattern::from_rrule(r, start_day, tz) {
            Some(p) => (Some(p), Vec::new()),
            None => match recurrence::expand(r, start_day, tz) {
                Some(days) if days.len() > 1 => {
                    let (p, gaps) = Pattern::covering(&days, r.wkst);
                    (Some(p), gaps)
                }
                _ => (None, Vec::new()),
            },
        },
        None => (None, Vec::new()),
    };
    let duration_secs = (end.utc - start.utc).max(0);
    let mut first = start;
    let mut exceptions: Vec<SeriesException> = Vec::new();
    if let (Some(rule), Some(pattern)) = (&rule, &pattern) {
        let Some(first_day) = pattern.first_occurrence(start_day) else {
            return false;
        };
        let shift = (first_day - start_day) * 86_400;
        first = When {
            utc: tz.map_or(start.utc + shift, |z| z.wall_to_utc(start.wall + shift)),
            wall: start.wall + shift,
            all_day: start.all_day,
        };
        let start_minute = (start.wall.rem_euclid(86_400) / 60) as u32;
        let duration_minutes = (duration_secs / 60) as u32;
        let deleted: Vec<i64> = ev
            .props("EXDATE")
            .flat_map(|x| {
                x.value
                    .split(',')
                    .filter_map(|v| {
                        let w = match parse_time(v)? {
                            Time::Utc(t) => tz.map_or(t, |z| z.utc_to_wall(t)),
                            Time::Wall(t) => t,
                            Time::Date(d) => d * 86_400,
                        };
                        Some(w.div_euclid(86_400))
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        let mut deleted = deleted;
        deleted.extend(&covered_gaps);
        if class == CLASS_REQUEST {
            for o in events
                .iter()
                .skip(1)
                .filter(|o| o.text("UID").unwrap_or_default() == uid)
            {
                match series_exception(o, ev, &zones, tz, duration_secs) {
                    Some(Override::Cancelled(day)) => deleted.push(day),
                    Some(Override::Modified(x)) => exceptions.push(x),
                    None => {}
                }
            }
        }
        let infos: Vec<Exception> = exceptions.iter().map(|x| x.info.clone()).collect();
        p.appt(
            LID_RECUR,
            PropValue::Binary(pattern.blob(
                first_day,
                start_minute,
                duration_minutes,
                &deleted,
                &infos,
            )),
        );
        p.appt(
            LID_RECURRENCE_PATTERN,
            PropValue::Unicode(describe(
                rule,
                start_minute,
                start_minute + duration_minutes,
            )),
        );
        p.appt(
            LID_TIME_ZONE_STRUCT,
            PropValue::Binary(wzone.time_zone_struct()),
        );
        p.appt(
            LID_TZDEF_RECUR,
            PropValue::Binary(wzone.definition(&tz_name, true)),
        );
        p.appt(LID_CLIP_START, ft(first_day * 86_400));
        let clip_end = pattern
            .last_day(first_day)
            .map_or(CLIP_END_NEVER, |d| unix_to_filetime(d * 86_400));
        p.appt(LID_CLIP_END, PropValue::Time(clip_end));
    } else {
        p.appt(LID_CLIP_START, ft(start.utc));
        p.appt(LID_CLIP_END, ft(end.utc));
    }
    let recurring = pattern.is_some();
    p.appt(LID_RECURRING, PropValue::Boolean(recurring));
    p.meeting(
        LID_IS_RECURRING,
        PropValue::Boolean(recurring || recurrence_id.is_some()),
    );
    p.meeting(
        LID_IS_EXCEPTION,
        PropValue::Boolean(recurrence_id.is_some()),
    );
    p.appt(
        LID_TIME_ZONE_DESCRIPTION,
        PropValue::Unicode(tz_name.clone()),
    );
    p.appt(
        LID_TZDEF_START_DISPLAY,
        PropValue::Binary(wzone.definition(&tz_name, false)),
    );
    p.appt(
        LID_TZDEF_END_DISPLAY,
        PropValue::Binary(wzone.definition(&tz_name, false)),
    );

    let (first_end_utc, first_start_utc) = (first.utc + duration_secs, first.utc);
    p.appt(LID_START_WHOLE, ft(first_start_utc));
    p.appt(LID_END_WHOLE, ft(first_end_utc));
    p.common(LID_COMMON_START, ft(first_start_utc));
    p.common(LID_COMMON_END, ft(first_end_utc));
    p.tag(PR_START_DATE, ft(first_start_utc));
    p.tag(PR_END_DATE, ft(first_end_utc));
    p.appt(
        LID_DURATION,
        PropValue::Integer32((duration_secs / 60) as i32),
    );
    p.appt(LID_SUB_TYPE, PropValue::Boolean(start.all_day));

    // The recipient's own status (tentative until they respond) and the one
    // the organizer intends; a canceled meeting no longer blocks time.
    let (busy, intended) = if canceled {
        (BUSY_FREE, BUSY_FREE)
    } else {
        let intended = busy_status(ev, "X-MICROSOFT-CDO-INTENDEDSTATUS");
        let busy = if ev.prop("X-MICROSOFT-CDO-BUSYSTATUS").is_some() {
            busy_status(ev, "X-MICROSOFT-CDO-BUSYSTATUS")
        } else {
            intended
        };
        (busy, intended)
    };
    p.appt(LID_BUSY_STATUS, PropValue::Integer32(busy));
    p.appt(LID_INTENDED_BUSY_STATUS, PropValue::Integer32(intended));
    let mut state = ASF_MEETING | ASF_RECEIVED;
    if canceled {
        state |= ASF_CANCELED;
    }
    p.appt(LID_STATE_FLAGS, PropValue::Integer32(state));
    let request = class == CLASS_REQUEST;
    p.appt(
        LID_RESPONSE_STATUS,
        PropValue::Integer32(if request {
            RESP_NOT_RESPONDED
        } else {
            RESP_NONE
        }),
    );
    p.appt(LID_FINVITED, PropValue::Boolean(request));
    if request {
        p.meeting(
            LID_MEETING_TYPE,
            PropValue::Integer32(if sequence > 0 {
                MTG_FULL_UPDATE
            } else {
                MTG_REQUEST
            }),
        );
        let rsvp = attendees.iter().any(|a| {
            a.param("RSVP")
                .is_some_and(|r| r.eq_ignore_ascii_case("TRUE"))
        });
        p.tag(PR_RESPONSE_REQUESTED, PropValue::Boolean(rsvp));
    }
    if method == "COUNTER" {
        p.appt(LID_COUNTER_PROPOSAL, PropValue::Boolean(true));
        p.appt(LID_PROPOSED_START, ft(start.utc));
        p.appt(LID_PROPOSED_END, ft(end.utc));
    }

    // Attendee lists: required attendees are To, optional ones Cc.
    let names = |pred: &dyn Fn(&str) -> bool| {
        attendees
            .iter()
            .filter(|a| {
                pred(
                    &a.param("ROLE")
                        .unwrap_or("REQ-PARTICIPANT")
                        .to_ascii_uppercase(),
                )
            })
            .map(|a| attendee_name(a))
            .collect::<Vec<_>>()
            .join("; ")
    };
    p.appt(LID_ALL_ATTENDEES, PropValue::Unicode(names(&|_| true)));
    p.appt(
        LID_TO_ATTENDEES,
        PropValue::Unicode(names(&|r| r != "OPT-PARTICIPANT" && r != "NON-PARTICIPANT")),
    );
    p.appt(
        LID_CC_ATTENDEES,
        PropValue::Unicode(names(&|r| r == "OPT-PARTICIPANT")),
    );

    // A reminder from the first VALARM with a relative trigger.
    let reminder = ev
        .children("VALARM")
        .filter_map(|a| a.value("TRIGGER").and_then(parse_duration))
        .next();
    p.common(
        LID_REMINDER_SET,
        PropValue::Boolean(request && reminder.is_some()),
    );
    let delta = reminder.map_or(15, |secs| (-secs / 60) as i32);
    p.common(LID_REMINDER_DELTA, PropValue::Integer32(delta));
    p.common(LID_REMINDER_TIME, ft(first_start_utc));
    p.common(
        LID_REMINDER_SIGNAL_TIME,
        ft(first_start_utc - i64::from(delta) * 60),
    );
    p.common(LID_SIDE_EFFECTS, PropValue::Integer32(SIDE_EFFECTS_MEETING));
    match ev
        .value("CLASS")
        .map(|c| c.trim().to_ascii_uppercase())
        .as_deref()
    {
        Some("PRIVATE") => p.tag(PR_SENSITIVITY, PropValue::Integer32(2)),
        Some("CONFIDENTIAL") => p.tag(PR_SENSITIVITY, PropValue::Integer32(3)),
        _ => {}
    }

    if msg.body.is_none() {
        msg.body = ev.text("DESCRIPTION");
    }
    // Outlook keeps "Accepted: " and the like as the subject prefix, so the
    // response threads with the meeting.
    if let Some(subject) = &msg.subject {
        const PREFIXES: [&str; 5] = [
            "Accepted:",
            "Declined:",
            "Tentative:",
            "Canceled:",
            "Cancelled:",
        ];
        if let Some(prefix) = PREFIXES.iter().find(|pre| {
            subject
                .get(..pre.len())
                .is_some_and(|s| s.eq_ignore_ascii_case(pre))
        }) {
            let rest = subject[prefix.len()..].trim_start();
            let prefix = &subject[..subject.len() - rest.len()];
            p.tag(PR_SUBJECT_PREFIX, PropValue::Unicode(prefix.to_owned()));
            p.tag(PR_NORMALIZED_SUBJECT, PropValue::Unicode(rest.to_owned()));
            p.tag(PR_CONVERSATION_TOPIC, PropValue::Unicode(rest.to_owned()));
        }
    }
    msg.message_class = Some(class.to_owned());
    msg.extra_props.extend(p.0);
    // The calendar parts are now the meeting object itself, unless the
    // recurrence could not be expressed; then the .ics keeps the full rule.
    if rule.is_none() || pattern.is_some() {
        msg.attachments.retain(|a| !is_calendar(a));
    }
    msg.attachments
        .extend(exceptions.iter().map(|x| x.attachment(start.all_day)));
    true
}

/// A modified occurrence: its recurrence-blob entry and what its exception
/// attachment needs.
struct SeriesException {
    info: Exception,
    start_utc: i64,
    end_utc: i64,
    original_utc: i64,
    subject: String,
    location: String,
    busy_status: i32,
}

enum Override {
    /// An occurrence removed by a STATUS:CANCELLED override (its local day).
    Cancelled(i64),
    Modified(SeriesException),
}

/// Reads an override VEVENT (one with RECURRENCE-ID) of the series `master`.
/// Its times are converted to the series' time zone.
fn series_exception(
    o: &Component,
    master: &Component,
    zones: &Zones,
    tz: Option<&TimeZone>,
    duration_secs: i64,
) -> Option<Override> {
    let series_wall = |w: When| {
        if w.all_day {
            w.wall
        } else {
            tz.map_or(w.utc, |z| z.utc_to_wall(w.utc))
        }
    };
    let original = zones.resolve(o.prop("RECURRENCE-ID")?)?;
    let original_wall = series_wall(original);
    let original_day = original_wall.div_euclid(86_400);
    if o.value("STATUS")
        .is_some_and(|s| s.trim().eq_ignore_ascii_case("CANCELLED"))
    {
        return Some(Override::Cancelled(original_day));
    }
    let start = o
        .prop("DTSTART")
        .and_then(|p| zones.resolve(p))
        .unwrap_or(original);
    let end = o
        .prop("DTEND")
        .and_then(|p| zones.resolve(p))
        .unwrap_or(When {
            utc: start.utc + duration_secs,
            wall: start.wall + duration_secs,
            all_day: start.all_day,
        });
    let subject = o
        .text("SUMMARY")
        .or_else(|| master.text("SUMMARY"))
        .unwrap_or_default();
    let location = o
        .text("LOCATION")
        .or_else(|| master.text("LOCATION"))
        .unwrap_or_default();
    let busy_status = busy_status(o, "X-MICROSOFT-CDO-BUSYSTATUS");
    let changed = |mine: &str, series: Option<String>| {
        (Some(mine) != series.as_deref()).then(|| mine.to_owned())
    };
    Some(Override::Modified(SeriesException {
        info: Exception {
            original_day,
            original_start: original_wall,
            start: series_wall(start),
            end: series_wall(end),
            subject: changed(&subject, master.text("SUMMARY")),
            location: changed(&location, master.text("LOCATION")),
            busy_status: (busy_status != busy_status_of(master)).then_some(busy_status),
        },
        start_utc: start.utc,
        end_utc: end.utc,
        original_utc: original.utc,
        subject,
        location,
        busy_status,
    }))
}

fn busy_status_of(ev: &Component) -> i32 {
    busy_status(ev, "X-MICROSOFT-CDO-BUSYSTATUS")
}

/// Message class of the exception objects embedded in a recurring meeting.
const CLASS_EXCEPTION: &str = "IPM.OLE.CLASS.{00061055-0000-0000-C000-000000000046}";
const LID_FEXCEPTIONAL_BODY: u32 = 0x8206;
const LID_EXCEPTION_REPLACE_TIME: u32 = 0x8228;
const LID_FEXCEPTIONAL_ATTENDEES: u32 = 0x822B;
const PR_EXCEPTION_REPLACE_TIME: u16 = 0x7FF9;
const PR_EXCEPTION_START_TIME: u16 = 0x7FFB;
const PR_EXCEPTION_END_TIME: u16 = 0x7FFC;
const PR_ATTACHMENT_FLAGS: u16 = 0x7FFD;
const AF_EXCEPTION: i32 = 0x2;

impl SeriesException {
    /// The hidden attachment holding the exception object (MS-OXOCAL 2.2.10).
    fn attachment(&self, all_day: bool) -> Attachment {
        let ft = |t: i64| PropValue::Time(unix_to_filetime(t));
        let mut p = Props(Vec::new());
        p.appt(LID_START_WHOLE, ft(self.start_utc));
        p.appt(LID_END_WHOLE, ft(self.end_utc));
        p.common(LID_COMMON_START, ft(self.start_utc));
        p.common(LID_COMMON_END, ft(self.end_utc));
        p.tag(PR_START_DATE, ft(self.start_utc));
        p.tag(PR_END_DATE, ft(self.end_utc));
        p.appt(LID_EXCEPTION_REPLACE_TIME, ft(self.original_utc));
        p.appt(LID_FEXCEPTIONAL_BODY, PropValue::Boolean(false));
        p.appt(LID_FEXCEPTIONAL_ATTENDEES, PropValue::Boolean(false));
        p.appt(LID_LOCATION, PropValue::Unicode(self.location.clone()));
        p.appt(LID_BUSY_STATUS, PropValue::Integer32(self.busy_status));
        p.appt(LID_SUB_TYPE, PropValue::Boolean(all_day));
        let inner = Message {
            message_class: Some(CLASS_EXCEPTION.to_owned()),
            subject: Some(self.subject.clone()),
            read: true,
            extra_props: p.0,
            ..Default::default()
        };
        Attachment {
            inline: true,
            embedded: Some(Box::new(inner)),
            extra_props: vec![
                (
                    PropKey::Id(PR_ATTACHMENT_FLAGS),
                    PropValue::Integer32(AF_EXCEPTION),
                ),
                // The exception's start and end in local time, and the
                // original occurrence it replaces in UTC.
                (PropKey::Id(PR_EXCEPTION_START_TIME), ft(self.info.start)),
                (PropKey::Id(PR_EXCEPTION_END_TIME), ft(self.info.end)),
                (
                    PropKey::Id(PR_EXCEPTION_REPLACE_TIME),
                    ft(self.original_utc),
                ),
            ],
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_object_id_matches_outlook() {
        // PidLidGlobalObjectId Outlook stored for a Google Calendar UID.
        let goid = global_object_id("4ak7hu2a2crsgek16gomltqisg@google.com", None);
        let hex: String = goid.iter().map(|b| format!("{b:02X}")).collect();
        assert_eq!(
            hex,
            "040000008200E00074C5B7101A82E0080000000000000000000000000000000000000000320000007643616C2D5569640100000034616B37687532613263727367656B3136676F6D6C747169736740676F6F676C652E636F6D00"
        );
        // An Exchange UID is the hex of a global object ID.
        let exchange = "040000008200E00074C5B7101A82E00800000000D0B1F2C1A4D8DA01000000000000000010000000AABBCCDDEEFF00112233445566778899";
        let goid = global_object_id(exchange, Some(ical::days_from_civil(2024, 7, 9)));
        assert_eq!(&goid[16..20], &[0x07, 0xE8, 7, 9]);
        assert_eq!(goid.len(), exchange.len() / 2);
    }
}
