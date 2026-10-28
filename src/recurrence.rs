//! Outlook recurrence and time zone structures (MS-OXOCAL 2.2.1.39-2.2.1.44)
//! built from iCalendar recurrence rules and VTIMEZONE definitions.

use crate::ical::{
    DAYS_1601_TO_1970, Freq, Observance, RRule, Time, TimeZone, civil_from_days, days_from_civil,
    days_in_month, weekday,
};

const MINUTES_PER_DAY: i64 = 1440;
const MINUTES_PER_WEEK: i64 = 10_080;

const FREQ_DAILY: u16 = 0x200A;
const FREQ_WEEKLY: u16 = 0x200B;
const FREQ_MONTHLY: u16 = 0x200C;
const FREQ_YEARLY: u16 = 0x200D;

const PATTERN_DAY: u16 = 0x0000;
const PATTERN_WEEK: u16 = 0x0001;
const PATTERN_MONTH: u16 = 0x0002;
const PATTERN_MONTH_NTH: u16 = 0x0003;

const END_AFTER_DATE: u32 = 0x2021;
const END_AFTER_N: u32 = 0x2022;
const END_NEVER: u32 = 0x2023;
const NEVER_END_DATE: u32 = 0x5AE9_80DF;
/// OccurrenceCount for a series without an end; the value carries no meaning.
const NEVER_END_COUNT: u32 = 10;
/// Recurrences are enumerated at most this far, for COUNT and UNTIL ends.
const MAX_SPAN_DAYS: i64 = 366 * 100;

/// Minutes from 1601-01-01 to midnight of a day (days since 1970).
pub fn minutes_1601(days: i64) -> u32 {
    ((days + DAYS_1601_TO_1970) * MINUTES_PER_DAY) as u32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Daily,
    /// Weekday bit mask, Sunday = 0x01 ... Saturday = 0x40.
    Weekly(u32),
    /// Day of the month, in every `interval` months (or one month of the year).
    Monthly {
        month: Option<u32>,
        day: u32,
    },
    /// The `nth` (5 = last) of the weekdays in `mask`.
    MonthlyNth {
        month: Option<u32>,
        mask: u32,
        nth: u32,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    kind: Kind,
    /// Days, weeks or months between recurrences.
    interval: u32,
    first_dow: u32,
    end: End,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum End {
    Never,
    Count(u32),
    /// Last day (days since 1970, local) an occurrence may fall on.
    Until(i64),
}

fn mask_of(days: impl IntoIterator<Item = u32>) -> u32 {
    days.into_iter().fold(0, |m, d| m | (1 << d))
}

impl Pattern {
    /// Maps a recurrence rule starting on local day `start` to an Outlook
    /// pattern. Rules Outlook cannot express return `None`.
    pub fn from_rrule(rule: &RRule, start: i64, tz: Option<&TimeZone>) -> Option<Pattern> {
        if rule.unsupported {
            return None;
        }
        let (_, start_month, start_day) = civil_from_days(start);
        let plain_days: Vec<u32> = rule
            .by_day
            .iter()
            .filter(|(o, _)| *o == 0)
            .map(|(_, d)| *d)
            .collect();
        let all_plain = plain_days.len() == rule.by_day.len();
        let nth = |rule: &RRule| -> Option<(u32, u32)> {
            match (rule.by_day.as_slice(), rule.by_set_pos.as_slice()) {
                ([(ord, wd)], []) if *ord != 0 => Some((mask_of([*wd]), ordinal(*ord)?)),
                (days, [pos]) if all_plain && !days.is_empty() => {
                    Some((mask_of(plain_days.clone()), ordinal(*pos)?))
                }
                _ => None,
            }
        };
        let single_month = |rule: &RRule| match rule.by_month.as_slice() {
            [] => Some(start_month),
            [m] => Some(*m),
            _ => None,
        };
        let month_day = |rule: &RRule| match rule.by_month_day.as_slice() {
            [] => Some(start_day),
            [d] if (1..=31).contains(d) => Some(*d as u32),
            _ => None,
        };

        let (kind, interval) = match rule.freq {
            Freq::Daily
                if rule.by_day.is_empty()
                    && rule.by_month_day.is_empty()
                    && rule.by_month.is_empty() =>
            {
                (Kind::Daily, rule.interval)
            }
            // "Every weekday" and similar are weekly patterns in Outlook.
            Freq::Daily
                if rule.interval == 1
                    && all_plain
                    && !plain_days.is_empty()
                    && rule.by_month.is_empty() =>
            {
                (Kind::Weekly(mask_of(plain_days.clone())), 1)
            }
            Freq::Weekly
                if all_plain && rule.by_month.is_empty() && rule.by_month_day.is_empty() =>
            {
                let days = if plain_days.is_empty() {
                    vec![weekday(start)]
                } else {
                    plain_days.clone()
                };
                (Kind::Weekly(mask_of(days)), rule.interval)
            }
            Freq::Monthly if rule.by_month.is_empty() => match nth(rule) {
                Some((mask, n)) => (
                    Kind::MonthlyNth {
                        month: None,
                        mask,
                        nth: n,
                    },
                    rule.interval,
                ),
                None if rule.by_day.is_empty() => (
                    Kind::Monthly {
                        month: None,
                        day: month_day(rule)?,
                    },
                    rule.interval,
                ),
                None => return None,
            },
            Freq::Yearly => {
                let month = Some(single_month(rule)?);
                match nth(rule) {
                    Some((mask, n)) => (
                        Kind::MonthlyNth {
                            month,
                            mask,
                            nth: n,
                        },
                        12 * rule.interval,
                    ),
                    None if rule.by_day.is_empty() => (
                        Kind::Monthly {
                            month,
                            day: month_day(rule)?,
                        },
                        12 * rule.interval,
                    ),
                    None => return None,
                }
            }
            _ => return None,
        };

        let end = match (rule.count, rule.until) {
            (Some(n), _) => End::Count(n),
            (None, Some(until)) => End::Until(match until {
                Time::Date(d) => d,
                Time::Wall(t) => t.div_euclid(86_400),
                Time::Utc(t) => tz.map_or(t, |tz| tz.utc_to_wall(t)).div_euclid(86_400),
            }),
            (None, None) => End::Never,
        };
        Some(Pattern {
            kind,
            interval,
            first_dow: rule.wkst,
            end,
        })
    }

    pub fn is_daily(&self) -> bool {
        self.kind == Kind::Daily
    }

    fn week_start(&self, day: i64) -> i64 {
        day - i64::from((weekday(day) + 7 - self.first_dow) % 7)
    }

    fn months(day: i64) -> i64 {
        let (y, m, _) = civil_from_days(day);
        y * 12 + i64::from(m) - 1
    }

    /// Whether `day` is an occurrence of a series whose first occurrence is
    /// on `first` (both local days since 1970).
    fn matches(&self, day: i64, first: i64) -> bool {
        if day < first {
            return false;
        }
        let interval = i64::from(self.interval);
        let (y, m, d) = civil_from_days(day);
        let month_ok = |month: Option<u32>| {
            month.is_none_or(|mm| mm == m)
                && (Self::months(day) - Self::months(first)) % interval == 0
        };
        match self.kind {
            Kind::Daily => (day - first) % interval == 0,
            Kind::Weekly(mask) => {
                mask & (1 << weekday(day)) != 0
                    && ((self.week_start(day) - self.week_start(first)) / 7) % interval == 0
            }
            Kind::Monthly { month, day: md } => {
                // Outlook moves a day past the end of a month to its last day.
                d == md.min(days_in_month(y, m)) && month_ok(month)
            }
            Kind::MonthlyNth { month, mask, nth } => {
                if !month_ok(month) || mask & (1 << weekday(day)) == 0 {
                    return false;
                }
                // The nth (or last) day of the month whose weekday is in the mask.
                let first_of_month = days_from_civil(y, m, 1);
                let len = i64::from(days_in_month(y, m));
                let candidates: Vec<i64> = (first_of_month..first_of_month + len)
                    .filter(|&x| mask & (1 << weekday(x)) != 0)
                    .collect();
                let pick = if nth == 5 {
                    candidates.last()
                } else {
                    candidates.get(nth as usize - 1)
                };
                pick == Some(&day)
            }
        }
    }

    /// The first occurrence on or after `start`.
    pub fn first_occurrence(&self, start: i64) -> Option<i64> {
        // The series is anchored at `start` for interval arithmetic.
        (start..start + MAX_SPAN_DAYS).find(|&d| self.matches(d, start))
    }

    /// The last occurrence and the number of occurrences, for bounded series.
    fn last_occurrence(&self, first: i64) -> Option<(i64, u32)> {
        let (limit_days, limit_count) = match self.end {
            End::Never => return None,
            End::Count(n) => (first + MAX_SPAN_DAYS, n),
            End::Until(u) => (u, u32::MAX),
        };
        let mut last = None;
        let mut count = 0;
        for d in first..=limit_days.min(first + MAX_SPAN_DAYS) {
            if self.matches(d, first) {
                count += 1;
                last = Some(d);
                if count == limit_count {
                    break;
                }
            }
        }
        last.map(|l| (l, count))
    }

    pub fn occurs_on(&self, day: i64, first: i64) -> bool {
        self.matches(day, first)
    }

    /// The day of the last occurrence, if the series ends.
    pub fn last_day(&self, first: i64) -> Option<i64> {
        self.last_occurrence(first).map(|(d, _)| d)
    }

    /// PidLidAppointmentRecur: an AppointmentRecurrencePattern. `first` is the
    /// first occurrence's local day, `start_minute` its start in minutes after
    /// local midnight; `deleted` holds local days of removed occurrences and
    /// `exceptions` the modified ones.
    pub fn blob(
        &self,
        first: i64,
        start_minute: u32,
        duration_minutes: u32,
        deleted: &[i64],
        exceptions: &[Exception],
    ) -> Vec<u8> {
        let start_date = minutes_1601(first);
        let (freq, pattern_type) = match self.kind {
            Kind::Daily => (FREQ_DAILY, PATTERN_DAY),
            Kind::Weekly(_) => (FREQ_WEEKLY, PATTERN_WEEK),
            Kind::Monthly { month: Some(_), .. } if self.interval == 12 => {
                (FREQ_YEARLY, PATTERN_MONTH)
            }
            Kind::MonthlyNth { month: Some(_), .. } if self.interval == 12 => {
                (FREQ_YEARLY, PATTERN_MONTH_NTH)
            }
            Kind::Monthly { .. } => (FREQ_MONTHLY, PATTERN_MONTH),
            Kind::MonthlyNth { .. } => (FREQ_MONTHLY, PATTERN_MONTH_NTH),
        };
        let (period, first_date_time) = match self.kind {
            Kind::Daily => {
                let period = i64::from(self.interval) * MINUTES_PER_DAY;
                (period, i64::from(start_date) % period)
            }
            Kind::Weekly(_) => {
                let period = i64::from(self.interval);
                let week = i64::from(minutes_1601(self.week_start(first)));
                (period, week % (period * MINUTES_PER_WEEK))
            }
            _ => {
                // Months since January 1601 modulo the period, as the first
                // day of that month in minutes since 1601.
                let period = i64::from(self.interval);
                let offset = (Self::months(first) - 1601 * 12) % period;
                let (y, m) = (1601 + offset / 12, (offset % 12) as u32 + 1);
                (period, i64::from(minutes_1601(days_from_civil(y, m, 1))))
            }
        };

        let mut b = Vec::with_capacity(96);
        let w16 = |b: &mut Vec<u8>, v: u16| b.extend_from_slice(&v.to_le_bytes());
        let w32 = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
        w16(&mut b, 0x3004); // ReaderVersion
        w16(&mut b, 0x3004); // WriterVersion
        w16(&mut b, freq);
        w16(&mut b, pattern_type);
        w16(&mut b, 0); // CalendarType: Gregorian
        w32(&mut b, first_date_time as u32);
        w32(&mut b, period as u32);
        w32(&mut b, 0); // SlidingFlag
        match self.kind {
            Kind::Daily => {}
            Kind::Weekly(mask) => w32(&mut b, mask),
            Kind::Monthly { day, .. } => w32(&mut b, day),
            Kind::MonthlyNth { mask, nth, .. } => {
                w32(&mut b, mask);
                w32(&mut b, nth);
            }
        }
        let bounded = self.last_occurrence(first);
        let (end_type, count, end_date) = match (self.end, bounded) {
            (End::Count(_), Some((last, n))) => (END_AFTER_N, n, minutes_1601(last)),
            (End::Until(_), Some((last, n))) => (END_AFTER_DATE, n, minutes_1601(last)),
            _ => (END_NEVER, NEVER_END_COUNT, NEVER_END_DATE),
        };
        w32(&mut b, end_type);
        w32(&mut b, count);
        w32(&mut b, self.first_dow);
        // Every modified instance is also a deleted instance, by its original date.
        let mut deleted: Vec<u32> = deleted
            .iter()
            .chain(exceptions.iter().map(|e| &e.original_day))
            .filter(|&&d| self.matches(d, first))
            .map(|&d| minutes_1601(d))
            .collect();
        deleted.sort_unstable();
        deleted.dedup();
        w32(&mut b, deleted.len() as u32);
        for d in &deleted {
            w32(&mut b, *d);
        }
        let mut exceptions: Vec<&Exception> = exceptions
            .iter()
            .filter(|e| self.matches(e.original_day, first))
            .collect();
        exceptions.sort_by_key(|e| e.start);
        exceptions.dedup_by_key(|e| e.original_day);
        w32(&mut b, exceptions.len() as u32);
        for e in &exceptions {
            w32(&mut b, minutes_1601(e.start.div_euclid(86_400)));
        }
        w32(&mut b, start_date);
        w32(&mut b, end_date);
        w32(&mut b, 0x3006); // ReaderVersion2
        w32(&mut b, 0x3009); // WriterVersion2
        w32(&mut b, start_minute);
        w32(&mut b, start_minute + duration_minutes);
        w16(&mut b, exceptions.len() as u16);
        for e in &exceptions {
            e.write_info(&mut b);
        }
        w32(&mut b, 0); // ReservedBlock1Size
        for e in &exceptions {
            e.write_extended(&mut b);
        }
        w32(&mut b, 0); // ReservedBlock2Size
        b
    }
}

const ARO_SUBJECT: u16 = 0x0001;
const ARO_LOCATION: u16 = 0x0010;
const ARO_BUSYSTATUS: u16 = 0x0020;
const BIT_CH_START: u32 = 0x0000_0001;
const BIT_CH_END: u32 = 0x0000_0002;
const BIT_CH_LOCATION: u32 = 0x0000_0008;
const BIT_CH_SUBJECT: u32 = 0x0000_0010;

/// A modified instance of a recurring series. Times are local wall times
/// (seconds since 1970) in the series' time zone.
#[derive(Clone, Debug, PartialEq)]
pub struct Exception {
    pub original_day: i64,
    pub original_start: i64,
    pub start: i64,
    pub end: i64,
    /// Set only when they differ from the series.
    pub subject: Option<String>,
    pub location: Option<String>,
    pub busy_status: Option<i32>,
}

/// Minutes since 1601 of a wall time.
fn wall_minutes(wall: i64) -> u32 {
    (wall.div_euclid(60) + DAYS_1601_TO_1970 * MINUTES_PER_DAY) as u32
}

/// An 8-bit string for the ExceptionInfo fields: Windows-1252, with '?' for
/// characters it lacks (the Unicode form follows in ExtendedException).
fn ansi(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        let mut buf = [0u8; 4];
        let (bytes, _, errors) = encoding_rs::WINDOWS_1252.encode(c.encode_utf8(&mut buf));
        out.push(if errors || bytes.len() != 1 {
            b'?'
        } else {
            bytes[0]
        });
    }
    out
}

impl Exception {
    fn flags(&self) -> u16 {
        let mut f = 0;
        if self.subject.is_some() {
            f |= ARO_SUBJECT;
        }
        if self.location.is_some() {
            f |= ARO_LOCATION;
        }
        if self.busy_status.is_some() {
            f |= ARO_BUSYSTATUS;
        }
        f
    }

    fn times(&self, b: &mut Vec<u8>) {
        for t in [self.start, self.end, self.original_start] {
            b.extend_from_slice(&wall_minutes(t).to_le_bytes());
        }
    }

    /// ExceptionInfo (MS-OXOCAL 2.2.1.44.2). Overridden fields follow the
    /// flags in a fixed order: subject, (meeting type, reminder delta,
    /// reminder set,) location, busy status.
    fn write_info(&self, b: &mut Vec<u8>) {
        let ansi_field = |b: &mut Vec<u8>, text: &str| {
            let bytes = ansi(text);
            b.extend_from_slice(&(bytes.len() as u16 + 1).to_le_bytes());
            b.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
            b.extend_from_slice(&bytes);
        };
        self.times(b);
        b.extend_from_slice(&self.flags().to_le_bytes());
        if let Some(subject) = &self.subject {
            ansi_field(b, subject);
        }
        if let Some(location) = &self.location {
            ansi_field(b, location);
        }
        if let Some(busy) = self.busy_status {
            b.extend_from_slice(&busy.to_le_bytes());
        }
    }

    /// ExtendedException (MS-OXOCAL 2.2.1.44.3), for WriterVersion2 0x3009.
    fn write_extended(&self, b: &mut Vec<u8>) {
        let mut highlight = 0;
        if self.start != self.original_start {
            highlight |= BIT_CH_START | BIT_CH_END;
        }
        if self.location.is_some() {
            highlight |= BIT_CH_LOCATION;
        }
        if self.subject.is_some() {
            highlight |= BIT_CH_SUBJECT;
        }
        b.extend_from_slice(&4u32.to_le_bytes()); // ChangeHighlightSize
        b.extend_from_slice(&highlight.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes()); // ReservedBlockEE1Size
        if self.subject.is_some() || self.location.is_some() {
            self.times(b);
            for text in [&self.subject, &self.location].into_iter().flatten() {
                let units: Vec<u16> = text.encode_utf16().collect();
                b.extend_from_slice(&(units.len() as u16).to_le_bytes());
                b.extend(units.iter().flat_map(|u| u.to_le_bytes()));
            }
            b.extend_from_slice(&0u32.to_le_bytes()); // ReservedBlockEE2Size
        }
    }
}

/// Occurrences enumerated at most when expanding a rule.
const MAX_EXPANDED: usize = 5000;

/// The days of one period of a rule, before BYSETPOS (RFC 5545 3.3.10).
fn period_days(rule: &RRule, period_start: i64, start: i64) -> Option<Vec<i64>> {
    let (_, sm, sd) = civil_from_days(start);
    let month_days = |y: i64, m: u32| -> Option<Vec<i64>> {
        let first = days_from_civil(y, m, 1);
        let len = i64::from(days_in_month(y, m));
        let by_month_day: Vec<i64> = rule
            .by_month_day
            .iter()
            .filter_map(|&d| {
                let d = i64::from(d);
                let off = if d > 0 { d - 1 } else { len + d };
                (0..len).contains(&off).then_some(first + off)
            })
            .collect();
        let by_day: Vec<i64> = if rule.by_day.is_empty() {
            Vec::new()
        } else {
            let mut v = Vec::new();
            for &(ord, wd) in &rule.by_day {
                if ord == 0 {
                    v.extend((first..first + len).filter(|&x| weekday(x) == wd));
                } else {
                    v.extend(crate::ical::nth_weekday(y, m, wd, ord));
                }
            }
            v
        };
        Some(
            match (rule.by_month_day.is_empty(), rule.by_day.is_empty()) {
                (true, true) => {
                    if sd <= days_in_month(y, m) {
                        vec![days_from_civil(y, m, sd)]
                    } else {
                        Vec::new()
                    }
                }
                (false, true) => by_month_day,
                (true, false) => by_day,
                (false, false) => by_month_day
                    .into_iter()
                    .filter(|d| by_day.contains(d))
                    .collect(),
            },
        )
    };
    let month_ok =
        |d: i64| rule.by_month.is_empty() || rule.by_month.contains(&civil_from_days(d).1);
    let mut days = match rule.freq {
        Freq::Daily => {
            if rule.by_day.iter().any(|(o, _)| *o != 0) {
                return None;
            }
            let d = period_start;
            let (_, _, md) = civil_from_days(d);
            let len = i64::from(days_in_month(civil_from_days(d).0, civil_from_days(d).1));
            let md_ok = rule.by_month_day.is_empty()
                || rule.by_month_day.iter().any(|&x| {
                    if x > 0 {
                        i64::from(x) == i64::from(md)
                    } else {
                        len + i64::from(x) + 1 == i64::from(md)
                    }
                });
            let wd_ok = rule.by_day.is_empty() || rule.by_day.iter().any(|&(_, w)| w == weekday(d));
            if md_ok && wd_ok { vec![d] } else { Vec::new() }
        }
        Freq::Weekly => {
            if rule.by_day.iter().any(|(o, _)| *o != 0) || !rule.by_month_day.is_empty() {
                return None;
            }
            let wds: Vec<u32> = if rule.by_day.is_empty() {
                vec![weekday(start)]
            } else {
                rule.by_day.iter().map(|&(_, w)| w).collect()
            };
            (period_start..period_start + 7)
                .filter(|&d| wds.contains(&weekday(d)))
                .collect()
        }
        Freq::Monthly => {
            let (y, m, _) = civil_from_days(period_start);
            month_days(y, m)?
        }
        Freq::Yearly => {
            let y = civil_from_days(period_start).0;
            let ordinal_in_year =
                rule.by_month.is_empty() && rule.by_day.iter().any(|(o, _)| *o != 0);
            if ordinal_in_year {
                if !rule.by_month_day.is_empty() {
                    return None;
                }
                let (first, last) = (days_from_civil(y, 1, 1), days_from_civil(y, 12, 31));
                let mut v = Vec::new();
                for &(ord, wd) in &rule.by_day {
                    let all: Vec<i64> = (first..=last).filter(|&x| weekday(x) == wd).collect();
                    match ord {
                        0 => v.extend(&all),
                        n if n > 0 => v.extend(all.get(n as usize - 1)),
                        n => v.extend(
                            all.len()
                                .checked_sub((-n) as usize)
                                .and_then(|i| all.get(i)),
                        ),
                    }
                }
                v
            } else {
                let months: Vec<u32> = if !rule.by_month.is_empty() {
                    rule.by_month.clone()
                } else if !rule.by_month_day.is_empty() || !rule.by_day.is_empty() {
                    (1..=12).collect()
                } else {
                    vec![sm]
                };
                let mut v = Vec::new();
                for m in months {
                    v.extend(month_days(y, m)?);
                }
                v
            }
        }
    };
    days.retain(|&d| month_ok(d));
    days.sort_unstable();
    days.dedup();
    if !rule.by_set_pos.is_empty() {
        let all = std::mem::take(&mut days);
        for &pos in &rule.by_set_pos {
            let i = if pos > 0 {
                (pos - 1) as usize
            } else {
                all.len().wrapping_sub((-pos) as usize)
            };
            days.extend(all.get(i));
        }
        days.sort_unstable();
        days.dedup();
    }
    Some(days)
}

/// Expands a bounded rule (COUNT or UNTIL) into its occurrence days, the
/// first being the series start as RFC 5545 requires. Rules with parts this
/// expander does not model, and unbounded rules, return `None`.
pub fn expand(rule: &RRule, start: i64, tz: Option<&TimeZone>) -> Option<Vec<i64>> {
    if rule.unsupported {
        return None;
    }
    let until = match rule.until {
        Some(Time::Date(d)) => Some(d),
        Some(Time::Wall(t)) => Some(t.div_euclid(86_400)),
        Some(Time::Utc(t)) => Some(tz.map_or(t, |z| z.utc_to_wall(t)).div_euclid(86_400)),
        None => None,
    };
    if rule.count.is_none() && until.is_none() {
        return None;
    }
    let count = rule
        .count
        .map_or(MAX_EXPANDED, |c| (c as usize).min(MAX_EXPANDED));
    let interval = i64::from(rule.interval);
    let mut out = vec![start];
    let (y0, m0, _) = civil_from_days(start);
    let week0 = start - i64::from((weekday(start) + 7 - rule.wkst) % 7);
    for n in 0.. {
        let period_start = match rule.freq {
            Freq::Daily => start + n * interval,
            Freq::Weekly => week0 + 7 * n * interval,
            Freq::Monthly => {
                let months = y0 * 12 + i64::from(m0) - 1 + n * interval;
                days_from_civil(months.div_euclid(12), (months.rem_euclid(12) + 1) as u32, 1)
            }
            Freq::Yearly => days_from_civil(y0 + n * interval, 1, 1),
        };
        if period_start - start > MAX_SPAN_DAYS || until.is_some_and(|u| period_start > u + 366) {
            break;
        }
        for d in period_days(rule, period_start, start)? {
            if d <= start || until.is_some_and(|u| d > u) {
                continue;
            }
            out.push(d);
            if out.len() >= count {
                return Some(out);
            }
        }
    }
    Some(out)
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

impl Pattern {
    /// A daily pattern covering exactly `days` (ascending, first = start):
    /// every n-th day from the first to the last, with the days in between
    /// that are not occurrences returned as deletions.
    pub fn covering(days: &[i64], first_dow: u32) -> (Pattern, Vec<i64>) {
        let first = days[0];
        let last = *days.last().expect("non-empty");
        let step = days.iter().fold(0, |g, &d| gcd(g, d - first)).max(1);
        let pattern = Pattern {
            kind: Kind::Daily,
            interval: step as u32,
            first_dow,
            end: End::Until(last),
        };
        let set: std::collections::HashSet<i64> = days.iter().copied().collect();
        let deleted = (first..=last)
            .step_by(step as usize)
            .filter(|d| !set.contains(d))
            .collect();
        (pattern, deleted)
    }
}

/// An iCalendar ordinal as Outlook's week-of-month number: 1-4, or 5 for last.
fn ordinal(n: i32) -> Option<u32> {
    match n {
        1..=4 => Some(n as u32),
        -1 => Some(5),
        _ => None,
    }
}

/// A Windows time zone rule: offsets in minutes and the transition dates as
/// SYSTEMTIME fields (month, weekday, week of month, hour, minute).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowsZone {
    /// Minutes to add to local standard time to get UTC.
    pub bias: i32,
    pub daylight_bias: i32,
    pub standard_date: Option<[u16; 5]>,
    pub daylight_date: Option<[u16; 5]>,
}

fn transition(o: &Observance) -> Option<[u16; 5]> {
    let rule = o.rule.as_ref().filter(|r| r.freq == Freq::Yearly)?;
    let (_, start_month, _) = civil_from_days(o.start.div_euclid(86_400));
    let month = rule.by_month.first().copied().unwrap_or(start_month);
    let &(ord, wd) = rule.by_day.first()?;
    let week = ordinal(if ord == 0 { 1 } else { ord })?;
    let tod = o.start.rem_euclid(86_400);
    Some([
        month as u16,
        wd as u16,
        week as u16,
        (tod / 3600) as u16,
        (tod % 3600 / 60) as u16,
    ])
}

impl WindowsZone {
    pub const UTC: WindowsZone = WindowsZone {
        bias: 0,
        daylight_bias: 0,
        standard_date: None,
        daylight_date: None,
    };

    /// The rule a VTIMEZONE follows in `year`. Daylight saving that cannot be
    /// expressed as a yearly weekday rule is dropped.
    pub fn from_tz(tz: &TimeZone, year: i64) -> WindowsZone {
        let (standard, daylight) = tz.rules_for_year(year);
        let Some(standard) = standard.or(daylight) else {
            return WindowsZone::UTC;
        };
        let bias = -standard.offset_to / 60;
        match (
            daylight,
            transition(standard),
            daylight.and_then(transition),
        ) {
            (Some(d), Some(sd), Some(dd)) if d.offset_to != standard.offset_to => WindowsZone {
                bias,
                daylight_bias: -(d.offset_to - standard.offset_to) / 60,
                standard_date: Some(sd),
                daylight_date: Some(dd),
            },
            _ => WindowsZone {
                bias,
                ..WindowsZone::UTC
            },
        }
    }

    fn systemtime(date: Option<[u16; 5]>) -> [u8; 16] {
        let mut st = [0u8; 16];
        if let Some([month, dow, week, hour, minute]) = date {
            // wYear stays 0: a relative, yearly recurring date.
            for (i, v) in [0, month, dow, week, hour, minute, 0, 0]
                .into_iter()
                .enumerate()
            {
                st[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
            }
        }
        st
    }

    /// PidLidTimeZoneStruct (MS-OXOCAL 2.2.1.39).
    pub fn time_zone_struct(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(48);
        b.extend_from_slice(&self.bias.to_le_bytes());
        b.extend_from_slice(&0i32.to_le_bytes()); // lStandardBias
        b.extend_from_slice(&self.daylight_bias.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes()); // wStandardYear
        b.extend_from_slice(&Self::systemtime(self.standard_date));
        b.extend_from_slice(&0u16.to_le_bytes()); // wDaylightYear
        b.extend_from_slice(&Self::systemtime(self.daylight_date));
        b
    }

    /// A TZDEFINITION (MS-OXOCAL 2.2.1.41) with this single rule, effective
    /// for all years. `recur` marks it as the rule a recurring series uses.
    pub fn definition(&self, key_name: &str, recur: bool) -> Vec<u8> {
        let key: Vec<u16> = key_name.encode_utf16().take(260).collect();
        let mut b = vec![0x02, 0x01];
        // cbHeader counts the bytes after it, up to and including cRules.
        let header = 2 + 2 + 2 * key.len() + 2;
        b.extend_from_slice(&(header as u16).to_le_bytes());
        b.extend_from_slice(&0x0002u16.to_le_bytes()); // TZDEFINITION_FLAG_VALID_KEYNAME
        b.extend_from_slice(&(key.len() as u16).to_le_bytes());
        b.extend(key.iter().flat_map(|u| u.to_le_bytes()));
        b.extend_from_slice(&1u16.to_le_bytes()); // cRules
        b.extend_from_slice(&[0x02, 0x01]);
        b.extend_from_slice(&62u16.to_le_bytes()); // cbRule
        // TZRULE_FLAG_EFFECTIVE_TZREG, plus RECUR_CURRENT_TZREG for recurrences.
        let flags: u16 = if recur { 0x0003 } else { 0x0002 };
        b.extend_from_slice(&flags.to_le_bytes());
        // Effective from January 1, 1601: a SYSTEMTIME split into wYear and
        // the remaining 14 bytes.
        b.extend_from_slice(&1601u16.to_le_bytes());
        let mut rest = [0u8; 14];
        rest[0] = 1; // wMonth
        rest[4] = 1; // wDay
        b.extend_from_slice(&rest);
        b.extend_from_slice(&self.bias.to_le_bytes());
        b.extend_from_slice(&0i32.to_le_bytes()); // lStandardBias
        b.extend_from_slice(&self.daylight_bias.to_le_bytes());
        b.extend_from_slice(&Self::systemtime(self.standard_date));
        b.extend_from_slice(&Self::systemtime(self.daylight_date));
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ical::{parse, parse_rrule};

    fn u32_at(b: &[u8], off: usize) -> u32 {
        u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
    }

    #[test]
    fn first_date_time_weekly_follows_first_dow() {
        // MS-OXOCAL 2.2.1.44.1.1: 2007-04-12 (Thursday) lies in the week that
        // starts on Sunday 2007-04-08, or on Wednesday 2007-04-11.
        let start = days_from_civil(2007, 4, 12);
        for (wkst, week_start) in [
            ("SU", days_from_civil(2007, 4, 8)),
            ("WE", days_from_civil(2007, 4, 11)),
        ] {
            let rule = parse_rrule(&format!("FREQ=WEEKLY;BYDAY=TH;WKST={wkst}")).unwrap();
            let p = Pattern::from_rrule(&rule, start, None).unwrap();
            let blob = p.blob(start, 600, 60, &[], &[]);
            assert_eq!(u32_at(&blob, 10), minutes_1601(week_start) % 10_080);
        }
    }

    #[test]
    fn weekly_blob_layout() {
        let start = days_from_civil(2024, 10, 2); // a Wednesday
        let rule = parse_rrule("FREQ=WEEKLY;INTERVAL=2;BYDAY=WE;WKST=MO").unwrap();
        let p = Pattern::from_rrule(&rule, start, None).unwrap();
        let exdate = start + 14;
        let blob = p.blob(start, 9 * 60, 30, &[exdate, start + 7], &[]);
        assert_eq!(
            &blob[0..10],
            &[0x04, 0x30, 0x04, 0x30, 0x0B, 0x20, 0x01, 0x00, 0x00, 0x00]
        );
        assert_eq!(u32_at(&blob, 14), 2); // Period: weeks
        assert_eq!(u32_at(&blob, 22), 0x08); // Wednesday
        assert_eq!(u32_at(&blob, 26), END_NEVER);
        assert_eq!(u32_at(&blob, 34), 1); // FirstDOW: Monday
        // Only the day that is an occurrence counts as deleted.
        assert_eq!(u32_at(&blob, 38), 1);
        assert_eq!(u32_at(&blob, 42), minutes_1601(exdate));
        assert_eq!(u32_at(&blob, 46), 0); // ModifiedInstanceCount
        assert_eq!(u32_at(&blob, 50), minutes_1601(start));
        assert_eq!(u32_at(&blob, 54), NEVER_END_DATE);
        assert_eq!(u32_at(&blob, 58), 0x3006);
        assert_eq!(u32_at(&blob, 62), 0x3009);
        assert_eq!((u32_at(&blob, 66), u32_at(&blob, 70)), (540, 570));
        assert_eq!(blob.len(), 84);
    }

    #[test]
    fn counted_and_monthly_patterns() {
        let start = days_from_civil(2024, 1, 31);
        let rule = parse_rrule("FREQ=MONTHLY;COUNT=3").unwrap();
        let p = Pattern::from_rrule(&rule, start, None).unwrap();
        // Jan 31, Feb 29 (clamped), Mar 31.
        assert_eq!(p.last_day(start), Some(days_from_civil(2024, 3, 31)));
        let rule = parse_rrule("FREQ=MONTHLY;BYDAY=-1FR;UNTIL=20240601T000000Z").unwrap();
        let first = days_from_civil(2024, 1, 26);
        let p = Pattern::from_rrule(&rule, first, None).unwrap();
        assert_eq!(p.last_day(first), Some(days_from_civil(2024, 5, 31)));
        let blob = p.blob(first, 0, 60, &[], &[]);
        assert_eq!(&blob[4..8], &[0x0C, 0x20, 0x03, 0x00]);
        assert_eq!((u32_at(&blob, 22), u32_at(&blob, 26)), (0x20, 5));
        // Months since 1601-01 modulo 1 is 0: FirstDateTime is 1601-01-01.
        assert_eq!(u32_at(&blob, 10), 0);
        assert!(
            parse_rrule("FREQ=MONTHLY;BYDAY=MO,TU")
                .and_then(|r| Pattern::from_rrule(&r, first, None))
                .is_none()
        );
    }

    #[test]
    fn expands_rules_outlook_cannot_express() {
        let start = days_from_civil(2024, 1, 1);
        let rule = parse_rrule("FREQ=MONTHLY;BYMONTHDAY=1,15;COUNT=6").unwrap();
        assert!(Pattern::from_rrule(&rule, start, None).is_none());
        let days = expand(&rule, start, None).unwrap();
        let d = |m, day| days_from_civil(2024, m, day);
        assert_eq!(
            days,
            vec![d(1, 1), d(1, 15), d(2, 1), d(2, 15), d(3, 1), d(3, 15)]
        );
        let (p, deleted) = Pattern::covering(&days, 1);
        let occurs = |x: i64| {
            p.occurs_on(x, start)
                && !deleted.contains(&x)
                && p.last_day(start).is_some_and(|l| x <= l)
        };
        let covered: Vec<i64> = (start..d(4, 1)).filter(|&x| occurs(x)).collect();
        assert_eq!(covered, days);
        // Weekdays 1-2 of a month (BYDAY with BYSETPOS lists), until a date.
        let rule =
            parse_rrule("FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=1,2;UNTIL=20240331T000000Z")
                .unwrap();
        let days = expand(&rule, start, None).unwrap();
        assert_eq!(
            days,
            vec![d(1, 1), d(1, 2), d(2, 1), d(2, 2), d(3, 1), d(3, 4)]
        );
        // Unbounded and BYHOUR rules cannot be expanded.
        assert!(
            expand(
                &parse_rrule("FREQ=MONTHLY;BYMONTHDAY=1,15").unwrap(),
                start,
                None
            )
            .is_none()
        );
        assert!(
            expand(
                &parse_rrule("FREQ=DAILY;BYHOUR=9,17;COUNT=4").unwrap(),
                start,
                None
            )
            .is_none()
        );
        // Every 3rd day as a set gives a 3-day step and no deletions.
        let (p, deleted) = Pattern::covering(&[start, start + 3, start + 6], 1);
        assert_eq!((p.interval, deleted.len()), (3, 0));
    }

    #[test]
    fn windows_zone_from_vtimezone() {
        let cal = parse(
            "BEGIN:VTIMEZONE\r\nTZID:Romance Standard Time\r\nBEGIN:STANDARD\r\nDTSTART:16010101T030000\r\n\
TZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nRRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=10\r\nEND:STANDARD\r\n\
BEGIN:DAYLIGHT\r\nDTSTART:16010101T020000\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\n\
RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=3\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE\r\n",
        )
        .unwrap();
        let tz = TimeZone::from_component(&cal).unwrap();
        let wz = WindowsZone::from_tz(&tz, 2024);
        assert_eq!((wz.bias, wz.daylight_bias), (-60, -60));
        assert_eq!(wz.standard_date, Some([10, 0, 5, 3, 0]));
        assert_eq!(wz.daylight_date, Some([3, 0, 5, 2, 0]));
        // Byte for byte the TZDEFINITION Outlook wrote for this zone in a
        // meeting response (PidLidAppointmentTimeZoneDefinitionStartDisplay).
        assert_eq!(
            hex(&wz.definition("Central European Standard Time", false)),
            "0201420002001E00430065006E007400720061006C0020004500750072006F007000650061006E0020005300\
740061006E0064006100720064002000540069006D006500010002013E00020041060100000001000000000000000000\
C4FFFFFF00000000C4FFFFFF00000A0000000500030000000000000000000300000005000200000000000000"
        );
        assert_eq!(wz.time_zone_struct().len(), 48);
    }

    /// Reads an AppointmentRecurrencePattern in the order MFCMAPI's
    /// BinToAppointmentRecurrencePatternStruct does, returning the deleted
    /// and modified dates, the exceptions' (start, end, original, flags,
    /// subject, location) and the Unicode subjects, and requiring that every
    /// byte is consumed.
    #[allow(clippy::type_complexity)]
    fn read_pattern(
        b: &[u8],
    ) -> (
        Vec<u32>,
        Vec<u32>,
        Vec<(u32, u32, u32, u16, Option<Vec<u8>>, Option<Vec<u8>>)>,
        Vec<String>,
    ) {
        let mut pos = 0usize;
        let u16_ = |pos: &mut usize| {
            let v = u16::from_le_bytes(b[*pos..*pos + 2].try_into().unwrap());
            *pos += 2;
            v
        };
        let u32_ = |pos: &mut usize| {
            let v = u32::from_le_bytes(b[*pos..*pos + 4].try_into().unwrap());
            *pos += 4;
            v
        };
        u16_(&mut pos);
        u16_(&mut pos);
        u16_(&mut pos);
        let pattern_type = u16_(&mut pos);
        u16_(&mut pos);
        pos += 12; // FirstDateTime, Period, SlidingFlag
        pos += match pattern_type {
            0 => 0,
            3 | 0xB => 8,
            _ => 4,
        };
        pos += 12; // EndType, OccurrenceCount, FirstDOW
        let n = u32_(&mut pos);
        let deleted: Vec<u32> = (0..n).map(|_| u32_(&mut pos)).collect();
        let n = u32_(&mut pos);
        let modified: Vec<u32> = (0..n).map(|_| u32_(&mut pos)).collect();
        pos += 8; // StartDate, EndDate
        assert_eq!(u32_(&mut pos), 0x3006);
        let writer2 = u32_(&mut pos);
        pos += 8; // StartTimeOffset, EndTimeOffset
        let count = u16_(&mut pos) as usize;
        assert_eq!(count, modified.len());
        let mut infos = Vec::new();
        for _ in 0..count {
            let (start, end, orig) = (u32_(&mut pos), u32_(&mut pos), u32_(&mut pos));
            let flags = u16_(&mut pos);
            let text = |pos: &mut usize| {
                let len = u16_(pos) as usize;
                let len2 = u16_(pos) as usize;
                assert_eq!(len, len2 + 1);
                let t = b[*pos..*pos + len2].to_vec();
                *pos += len2;
                t
            };
            let subject = (flags & ARO_SUBJECT != 0).then(|| text(&mut pos));
            for flag in [0x2u16, 0x4, 0x8] {
                if flags & flag != 0 {
                    u32_(&mut pos);
                }
            }
            let location = (flags & ARO_LOCATION != 0).then(|| text(&mut pos));
            for flag in [0x20u16, 0x40, 0x80, 0x100] {
                if flags & flag != 0 {
                    u32_(&mut pos);
                }
            }
            infos.push((start, end, orig, flags, subject, location));
        }
        assert_eq!(u32_(&mut pos), 0); // ReservedBlock1Size
        let mut wide = Vec::new();
        for info in &infos {
            if writer2 >= 0x3009 {
                let size = u32_(&mut pos) as usize;
                pos += size;
            }
            assert_eq!(u32_(&mut pos), 0); // ReservedBlockEE1Size
            if info.3 & (ARO_SUBJECT | ARO_LOCATION) != 0 {
                assert_eq!(
                    (u32_(&mut pos), u32_(&mut pos), u32_(&mut pos)),
                    (info.0, info.1, info.2)
                );
            }
            for flag in [ARO_SUBJECT, ARO_LOCATION] {
                if info.3 & flag != 0 {
                    let n = u16_(&mut pos) as usize;
                    let units: Vec<u16> = (0..n)
                        .map(|i| u16::from_le_bytes([b[pos + 2 * i], b[pos + 2 * i + 1]]))
                        .collect();
                    pos += 2 * n;
                    wide.push(String::from_utf16(&units).unwrap());
                }
            }
            if info.3 & (ARO_SUBJECT | ARO_LOCATION) != 0 {
                assert_eq!(u32_(&mut pos), 0); // ReservedBlockEE2Size
            }
        }
        assert_eq!(u32_(&mut pos), 0); // ReservedBlock2Size
        assert_eq!(pos, b.len(), "no junk after the pattern");
        (deleted, modified, infos, wide)
    }

    #[test]
    fn exceptions_follow_mfcmapi_layout() {
        let first = days_from_civil(2024, 10, 7); // Monday
        let rule = parse_rrule("FREQ=WEEKLY;BYDAY=MO,WE;COUNT=6;WKST=MO").unwrap();
        let p = Pattern::from_rrule(&rule, first, None).unwrap();
        let moved = Exception {
            original_day: first + 2,
            original_start: (first + 2) * 86_400 + 9 * 3600,
            start: (first + 2) * 86_400 + 14 * 3600,
            end: (first + 2) * 86_400 + 14 * 3600 + 1800,
            subject: Some("Sync (moved) \u{2192} Ålborg".into()),
            location: Some("Room 7".into()),
            busy_status: None,
        };
        let renamed_only = Exception {
            original_day: first + 7,
            original_start: (first + 7) * 86_400 + 9 * 3600,
            start: (first + 7) * 86_400 + 9 * 3600,
            end: (first + 7) * 86_400 + 9 * 3600 + 1800,
            subject: None,
            location: None,
            busy_status: Some(0),
        };
        let blob = p.blob(
            first,
            540,
            30,
            &[first + 14],
            &[renamed_only.clone(), moved.clone()],
        );
        let (deleted, modified, infos, wide) = read_pattern(&blob);
        assert_eq!(
            deleted,
            vec![
                minutes_1601(first + 2),
                minutes_1601(first + 7),
                minutes_1601(first + 14)
            ]
        );
        assert_eq!(
            modified,
            vec![minutes_1601(first + 2), minutes_1601(first + 7)]
        );
        assert_eq!(infos[0].0, wall_minutes(moved.start));
        assert_eq!(infos[0].2, wall_minutes(moved.original_start));
        assert_eq!(infos[0].3, ARO_SUBJECT | ARO_LOCATION);
        assert_eq!(
            infos[0].4.as_deref(),
            Some(&b"Sync (moved) ? \xC5lborg"[..])
        );
        assert_eq!(infos[1].3, ARO_BUSYSTATUS);
        assert_eq!(
            wide,
            vec![
                "Sync (moved) \u{2192} Ålborg".to_owned(),
                "Room 7".to_owned()
            ]
        );
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02X}")).collect()
    }
}
