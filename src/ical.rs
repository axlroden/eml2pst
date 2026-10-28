//! A small iCalendar (RFC 5545) reader: content lines, components, the value
//! types meeting messages use, recurrence rules, and VTIMEZONE evaluation.
//!
//! Times are seconds since 1970-01-01. A "wall" time is a local clock reading
//! encoded the same way, as if it were UTC.

/// Days between 1601-01-01 and 1970-01-01.
pub const DAYS_1601_TO_1970: i64 = 134_774;

pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (
        if m <= 2 {
            yoe + era * 400 + 1
        } else {
            yoe + era * 400
        },
        m,
        d,
    )
}

/// Day of the week, 0 = Sunday.
pub fn weekday(days: i64) -> u32 {
    (days + 4).rem_euclid(7) as u32
}

pub fn days_in_month(y: i64, m: u32) -> u32 {
    let next = if m == 12 {
        days_from_civil(y + 1, 1, 1)
    } else {
        days_from_civil(y, m + 1, 1)
    };
    (next - days_from_civil(y, m, 1)) as u32
}

/// The `nth` (1-based, or negative from the end) `wd` weekday of a month.
pub fn nth_weekday(y: i64, m: u32, wd: u32, nth: i32) -> Option<i64> {
    let first = days_from_civil(y, m, 1);
    let len = i64::from(days_in_month(y, m));
    if nth > 0 {
        let d = first + i64::from((wd + 7 - weekday(first)) % 7) + 7 * (i64::from(nth) - 1);
        (d < first + len).then_some(d)
    } else {
        let last = first + len - 1;
        let d = last - i64::from((weekday(last) + 7 - wd) % 7) - 7 * (i64::from(-nth) - 1);
        (d >= first).then_some(d)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Property {
    pub name: String,
    pub params: Vec<(String, String)>,
    pub value: String,
}

impl Property {
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Component {
    pub name: String,
    pub props: Vec<Property>,
    pub children: Vec<Component>,
}

impl Component {
    pub fn prop(&self, name: &str) -> Option<&Property> {
        self.props
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
    }

    pub fn props<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Property> + 'a {
        self.props
            .iter()
            .filter(move |p| p.name.eq_ignore_ascii_case(name))
    }

    pub fn value(&self, name: &str) -> Option<&str> {
        self.prop(name).map(|p| p.value.as_str())
    }

    /// A TEXT value with escapes resolved.
    pub fn text(&self, name: &str) -> Option<String> {
        self.value(name).map(unescape)
    }

    pub fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Component> + 'a {
        self.children
            .iter()
            .filter(move |c| c.name.eq_ignore_ascii_case(name))
    }
}

pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push_str("\r\n"),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

fn parse_line(line: &str) -> Option<Property> {
    // The name and parameters end at the first colon outside quotes.
    let mut in_quotes = false;
    let mut split = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => {
                split = Some(i);
                break;
            }
            _ => {}
        }
    }
    let split = split?;
    let (head, value) = (&line[..split], &line[split + 1..]);
    let mut parts = Vec::new();
    let mut current = String::new();
    in_quotes = false;
    for c in head.chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            ';' if !in_quotes => parts.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    parts.push(current);
    let name = parts.remove(0).trim().to_ascii_uppercase();
    let params = parts
        .into_iter()
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            Some((k.trim().to_ascii_uppercase(), v.to_owned()))
        })
        .collect();
    Some(Property {
        name,
        params,
        value: value.to_owned(),
    })
}

/// Parses an iCalendar object and returns its first top-level component.
pub fn parse(text: &str) -> Option<Component> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        match (raw.strip_prefix([' ', '\t']), lines.last_mut()) {
            (Some(cont), Some(last)) => last.push_str(cont),
            _ if raw.trim().is_empty() => {}
            _ => lines.push(raw.to_owned()),
        }
    }
    let mut stack: Vec<Component> = Vec::new();
    for line in &lines {
        let Some(prop) = parse_line(line) else {
            continue;
        };
        match prop.name.as_str() {
            "BEGIN" => stack.push(Component {
                name: prop.value.trim().to_ascii_uppercase(),
                ..Default::default()
            }),
            "END" => {
                let done = stack.pop()?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(done),
                    None => return Some(done),
                }
            }
            _ => stack.last_mut()?.props.push(prop),
        }
    }
    // Tolerate a missing END:VCALENDAR.
    while stack.len() > 1 {
        let done = stack.pop()?;
        stack.last_mut()?.children.push(done);
    }
    stack.pop()
}

/// A DATE or DATE-TIME value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Time {
    /// An absolute time, seconds since 1970 UTC.
    Utc(i64),
    /// A wall-clock time, in the zone named by the property's TZID (or floating).
    Wall(i64),
    /// A date, days since 1970.
    Date(i64),
}

fn digits(s: &str, range: std::ops::Range<usize>) -> Option<u32> {
    s.get(range)?.parse().ok()
}

/// Parses one DATE (YYYYMMDD) or DATE-TIME (YYYYMMDDTHHMMSS[Z]) value.
pub fn parse_time(s: &str) -> Option<Time> {
    let s = s.trim();
    let days = days_from_civil(
        i64::from(digits(s, 0..4)?),
        digits(s, 4..6)?,
        digits(s, 6..8)?,
    );
    if s.len() == 8 {
        return Some(Time::Date(days));
    }
    if s.as_bytes().get(8) != Some(&b'T') {
        return None;
    }
    let secs = days * 86_400
        + i64::from(digits(s, 9..11)?) * 3600
        + i64::from(digits(s, 11..13)?) * 60
        + i64::from(digits(s, 13..15).unwrap_or(0));
    Some(if s.ends_with(['Z', 'z']) {
        Time::Utc(secs)
    } else {
        Time::Wall(secs)
    })
}

/// Parses a DURATION value into seconds.
pub fn parse_duration(s: &str) -> Option<i64> {
    let s = s.trim();
    let (sign, s) = match s.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, s.strip_prefix('+').unwrap_or(s)),
    };
    let s = s.strip_prefix('P')?;
    let mut total = 0i64;
    let mut num = String::new();
    let mut in_time = false;
    for c in s.chars() {
        match c {
            '0'..='9' => num.push(c),
            'T' => in_time = true,
            unit => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total += n * match (unit, in_time) {
                    ('W', _) => 604_800,
                    ('D', _) => 86_400,
                    ('H', true) => 3600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
            }
        }
    }
    Some(sign * total)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RRule {
    pub freq: Freq,
    pub interval: u32,
    pub count: Option<u32>,
    pub until: Option<Time>,
    /// (ordinal, weekday): ordinal 0 means every such weekday.
    pub by_day: Vec<(i32, u32)>,
    pub by_month_day: Vec<i32>,
    pub by_month: Vec<u32>,
    pub by_set_pos: Vec<i32>,
    /// Week start, 0 = Sunday.
    pub wkst: u32,
    /// Parts this reader does not model (BYHOUR, BYWEEKNO, ...).
    pub unsupported: bool,
}

fn weekday_code(s: &str) -> Option<u32> {
    Some(match s {
        "SU" => 0,
        "MO" => 1,
        "TU" => 2,
        "WE" => 3,
        "TH" => 4,
        "FR" => 5,
        "SA" => 6,
        _ => return None,
    })
}

pub fn parse_rrule(s: &str) -> Option<RRule> {
    let mut rule = RRule {
        freq: Freq::Daily,
        interval: 1,
        count: None,
        until: None,
        by_day: Vec::new(),
        by_month_day: Vec::new(),
        by_month: Vec::new(),
        by_set_pos: Vec::new(),
        wkst: 1,
        unsupported: false,
    };
    let mut freq = None;
    for part in s.trim().split(';').filter(|p| !p.is_empty()) {
        let (k, v) = part.split_once('=')?;
        let v = v.trim().to_ascii_uppercase();
        let ints = |v: &str| {
            v.split(',')
                .map(|x| x.trim().parse())
                .collect::<Result<Vec<i32>, _>>()
        };
        match k.trim().to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = Some(match v.as_str() {
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    "MONTHLY" => Freq::Monthly,
                    "YEARLY" => Freq::Yearly,
                    _ => return None,
                })
            }
            "INTERVAL" => rule.interval = v.parse().ok().filter(|&i| i > 0)?,
            "COUNT" => rule.count = Some(v.parse().ok()?),
            "UNTIL" => rule.until = Some(parse_time(&v)?),
            "BYDAY" => {
                for d in v.split(',') {
                    let d = d.trim();
                    let (ord, code) = d.split_at(d.len().checked_sub(2)?);
                    let ord = if ord.is_empty() { 0 } else { ord.parse().ok()? };
                    rule.by_day.push((ord, weekday_code(code)?));
                }
            }
            "BYMONTHDAY" => rule.by_month_day = ints(&v).ok()?,
            "BYMONTH" => rule.by_month = ints(&v).ok()?.into_iter().map(|m| m as u32).collect(),
            "BYSETPOS" => rule.by_set_pos = ints(&v).ok()?,
            "WKST" => rule.wkst = weekday_code(&v)?,
            _ => rule.unsupported = true,
        }
    }
    rule.freq = freq?;
    Some(rule)
}

/// One STANDARD or DAYLIGHT block of a VTIMEZONE.
#[derive(Clone, Debug, PartialEq)]
pub struct Observance {
    pub daylight: bool,
    /// Wall time (in `offset_from`) of the first onset.
    pub start: i64,
    pub offset_from: i32,
    pub offset_to: i32,
    pub rule: Option<RRule>,
    pub rdates: Vec<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimeZone {
    pub id: String,
    pub observances: Vec<Observance>,
}

fn parse_offset(s: &str) -> Option<i32> {
    let s = s.trim();
    let (sign, rest) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => (1, s),
    };
    let h: i32 = rest.get(0..2)?.parse().ok()?;
    let m: i32 = rest.get(2..4)?.parse().ok()?;
    let sec: i32 = rest.get(4..6).and_then(|x| x.parse().ok()).unwrap_or(0);
    Some(sign * (h * 3600 + m * 60 + sec))
}

impl TimeZone {
    pub fn from_component(c: &Component) -> Option<TimeZone> {
        let id = c.value("TZID")?.trim().trim_matches('"').to_owned();
        let mut observances = Vec::new();
        for o in &c.children {
            let daylight = match o.name.as_str() {
                "STANDARD" => false,
                "DAYLIGHT" => true,
                _ => continue,
            };
            let start = match parse_time(o.value("DTSTART")?)? {
                Time::Wall(t) | Time::Utc(t) => t,
                Time::Date(d) => d * 86_400,
            };
            let rdates = o
                .props("RDATE")
                .flat_map(|p| {
                    p.value
                        .split(',')
                        .filter_map(parse_time)
                        .collect::<Vec<_>>()
                })
                .map(|t| match t {
                    Time::Wall(t) | Time::Utc(t) => t,
                    Time::Date(d) => d * 86_400,
                })
                .collect();
            observances.push(Observance {
                daylight,
                start,
                offset_from: parse_offset(o.value("TZOFFSETFROM")?)?,
                offset_to: parse_offset(o.value("TZOFFSETTO")?)?,
                rule: o.value("RRULE").and_then(parse_rrule),
                rdates,
            });
        }
        (!observances.is_empty()).then_some(TimeZone { id, observances })
    }

    /// Onsets (wall time in the old offset) of an observance during `year`.
    fn onsets(o: &Observance, year: i64) -> Vec<i64> {
        let time_of_day = o.start.rem_euclid(86_400);
        let mut out: Vec<i64> = o.rdates.clone();
        out.push(o.start);
        if let Some(rule) = o.rule.as_ref().filter(|r| r.freq == Freq::Yearly) {
            let (_, start_month, start_day) = civil_from_days(o.start.div_euclid(86_400));
            let month = rule.by_month.first().copied().unwrap_or(start_month);
            let day = match (rule.by_day.first(), rule.by_month_day.first()) {
                (Some(&(ord, wd)), _) => {
                    nth_weekday(year, month, wd, if ord == 0 { 1 } else { ord })
                }
                (None, Some(&md)) => Some(days_from_civil(year, month, md as u32)),
                (None, None) => Some(days_from_civil(year, month, start_day)),
            };
            if let Some(day) = day {
                let t = day * 86_400 + time_of_day;
                let before_until = match rule.until {
                    Some(Time::Utc(u)) => t - i64::from(o.offset_from) <= u,
                    Some(Time::Wall(u)) => t <= u,
                    Some(Time::Date(d)) => t < (d + 1) * 86_400,
                    None => true,
                };
                if t >= o.start && before_until {
                    out.push(t);
                }
            }
        }
        out.retain(|&t| civil_from_days(t.div_euclid(86_400)).0 == year);
        out
    }

    /// The observance in effect at a wall time, by its latest onset not after it.
    fn observance_at_wall(&self, wall: i64) -> &Observance {
        let year = civil_from_days(wall.div_euclid(86_400)).0;
        let mut best: Option<(i64, &Observance)> = None;
        for o in &self.observances {
            for y in [year - 1, year] {
                for t in Self::onsets(o, y) {
                    if t <= wall && best.is_none_or(|(b, _)| t > b) {
                        best = Some((t, o));
                    }
                }
            }
        }
        best.map(|(_, o)| o).unwrap_or_else(|| {
            // Before the first onset: the earliest observance's starting offset.
            self.observances
                .iter()
                .min_by_key(|o| o.start)
                .expect("non-empty")
        })
    }

    pub fn offset_at_wall(&self, wall: i64) -> i32 {
        let o = self.observance_at_wall(wall);
        if wall < o.start {
            o.offset_from
        } else {
            o.offset_to
        }
    }

    pub fn wall_to_utc(&self, wall: i64) -> i64 {
        wall - i64::from(self.offset_at_wall(wall))
    }

    pub fn utc_to_wall(&self, utc: i64) -> i64 {
        // Guess with the offset at the UTC instant read as wall time, then
        // correct once; exact except within an hour of a transition.
        let guess = utc + i64::from(self.offset_at_wall(utc));
        utc + i64::from(self.offset_at_wall(guess))
    }

    /// The standard and daylight observances governing `year`, for building
    /// Windows time zone rules.
    pub fn rules_for_year(&self, year: i64) -> (Option<&Observance>, Option<&Observance>) {
        let latest = |daylight: bool| {
            self.observances
                .iter()
                .filter(|o| o.daylight == daylight)
                .filter(|o| civil_from_days(o.start.div_euclid(86_400)).0 <= year)
                .filter(|o| match o.rule.as_ref().and_then(|r| r.until) {
                    Some(Time::Utc(u) | Time::Wall(u)) => {
                        civil_from_days(u.div_euclid(86_400)).0 >= year
                    }
                    Some(Time::Date(d)) => civil_from_days(d).0 >= year,
                    None => true,
                })
                .max_by_key(|o| o.start)
        };
        (latest(false), latest(true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONDON: &str = "BEGIN:VCALENDAR\r\nBEGIN:VTIMEZONE\r\nTZID:GMT Standard Time\r\n\
BEGIN:STANDARD\r\nDTSTART:16010101T020000\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0000\r\n\
RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=10\r\nEND:STANDARD\r\n\
BEGIN:DAYLIGHT\r\nDTSTART:16010101T010000\r\nTZOFFSETFROM:+0000\r\nTZOFFSETTO:+0100\r\n\
RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=3\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nSUMMARY:Long \r\n line\\, with comma\r\nDTSTART;TZID=\"GMT Standard Time\":20240710T093000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn civil_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1601, 1, 1), -DAYS_1601_TO_1970);
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        assert_eq!(weekday(days_from_civil(2024, 10, 28)), 1); // a Monday
        assert_eq!(
            nth_weekday(2024, 3, 0, -1),
            Some(days_from_civil(2024, 3, 31))
        );
        assert_eq!(
            nth_weekday(2024, 10, 0, 2),
            Some(days_from_civil(2024, 10, 13))
        );
    }

    #[test]
    fn parses_components_and_text() {
        let cal = parse(LONDON).unwrap();
        let ev = cal.children("VEVENT").next().unwrap();
        assert_eq!(ev.text("SUMMARY").unwrap(), "Long line, with comma");
        assert_eq!(
            ev.prop("DTSTART").unwrap().param("TZID"),
            Some("GMT Standard Time")
        );
    }

    #[test]
    fn vtimezone_offsets() {
        let cal = parse(LONDON).unwrap();
        let tz = TimeZone::from_component(cal.children("VTIMEZONE").next().unwrap()).unwrap();
        let summer = days_from_civil(2024, 7, 10) * 86_400 + 9 * 3600 + 1800;
        assert_eq!(tz.wall_to_utc(summer), summer - 3600);
        let winter = days_from_civil(2024, 1, 10) * 86_400;
        assert_eq!(tz.wall_to_utc(winter), winter);
        // Daylight saving started 2024-03-31 01:00 UTC.
        let after = days_from_civil(2024, 3, 31) * 86_400 + 3 * 3600;
        assert_eq!(tz.offset_at_wall(after), 3600);
        let before = days_from_civil(2024, 3, 31) * 86_400;
        assert_eq!(tz.offset_at_wall(before), 0);
        assert_eq!(tz.utc_to_wall(summer - 3600), summer);
    }

    #[test]
    fn values() {
        assert_eq!(parse_duration("PT1H30M"), Some(5400));
        assert_eq!(parse_duration("-P1D"), Some(-86_400));
        assert_eq!(
            parse_time("20240101"),
            Some(Time::Date(days_from_civil(2024, 1, 1)))
        );
        assert_eq!(
            parse_time("20240101T100000Z"),
            Some(Time::Utc(days_from_civil(2024, 1, 1) * 86_400 + 36_000))
        );
        let r = parse_rrule("FREQ=WEEKLY;INTERVAL=2;BYDAY=WE,-1FR;WKST=MO").unwrap();
        assert_eq!((r.freq, r.interval, r.wkst), (Freq::Weekly, 2, 1));
        assert_eq!(r.by_day, vec![(0, 3), (-1, 5)]);
    }
}
