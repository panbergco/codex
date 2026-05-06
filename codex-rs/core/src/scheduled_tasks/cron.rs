use chrono::Datelike;
use chrono::Local;
use chrono::LocalResult;
use chrono::NaiveDateTime;
use chrono::TimeZone;
use chrono::Timelike;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CronFields {
    minute: Vec<u32>,
    hour: Vec<u32>,
    day_of_month: Vec<u32>,
    month: Vec<u32>,
    day_of_week: Vec<u32>,
}

#[derive(Debug, Clone, Copy)]
struct FieldRange {
    min: u32,
    max: u32,
}

const FIELD_RANGES: [FieldRange; 5] = [
    FieldRange { min: 0, max: 59 },
    FieldRange { min: 0, max: 23 },
    FieldRange { min: 1, max: 31 },
    FieldRange { min: 1, max: 12 },
    FieldRange { min: 0, max: 6 },
];

pub(crate) fn parse_cron_expression(expr: &str) -> Option<CronFields> {
    let parts = expr.split_whitespace().collect::<Vec<_>>();
    if parts.len() != 5 {
        return None;
    }

    let minute = expand_field(parts[0], FIELD_RANGES[0])?;
    let hour = expand_field(parts[1], FIELD_RANGES[1])?;
    let day_of_month = expand_field(parts[2], FIELD_RANGES[2])?;
    let month = expand_field(parts[3], FIELD_RANGES[3])?;
    let day_of_week = expand_field(parts[4], FIELD_RANGES[4])?;

    Some(CronFields {
        minute,
        hour,
        day_of_month,
        month,
        day_of_week,
    })
}

fn expand_field(field: &str, range: FieldRange) -> Option<Vec<u32>> {
    let mut out = std::collections::BTreeSet::new();
    for part in field.split(',') {
        if part.is_empty() {
            return None;
        }

        if let Some(step_part) = part.strip_prefix("*/") {
            let step = step_part.parse::<u32>().ok()?;
            if step == 0 {
                return None;
            }
            let mut value = range.min;
            while value <= range.max {
                out.insert(value);
                value += step;
            }
            continue;
        }

        if part == "*" {
            for value in range.min..=range.max {
                out.insert(value);
            }
            continue;
        }

        if let Some((span, step)) = part.split_once('/') {
            let step = step.parse::<u32>().ok()?;
            if step == 0 {
                return None;
            }
            expand_range(span, step, range, &mut out)?;
            continue;
        }

        if part.contains('-') {
            expand_range(part, 1, range, &mut out)?;
            continue;
        }

        let mut value = part.parse::<u32>().ok()?;
        if is_day_of_week(range) && value == 7 {
            value = 0;
        }
        if value < range.min || value > range.max {
            return None;
        }
        out.insert(value);
    }

    (!out.is_empty()).then(|| out.into_iter().collect())
}

fn expand_range(
    span: &str,
    step: u32,
    range: FieldRange,
    out: &mut std::collections::BTreeSet<u32>,
) -> Option<()> {
    let (lo, hi) = span.split_once('-')?;
    let lo = lo.parse::<u32>().ok()?;
    let hi = hi.parse::<u32>().ok()?;
    let effective_max = if is_day_of_week(range) { 7 } else { range.max };
    if lo > hi || lo < range.min || hi > effective_max {
        return None;
    }
    let mut value = lo;
    while value <= hi {
        out.insert(if is_day_of_week(range) && value == 7 {
            0
        } else {
            value
        });
        value += step;
    }
    Some(())
}

fn is_day_of_week(range: FieldRange) -> bool {
    range.min == 0 && range.max == 6
}

pub(crate) fn next_cron_run_ms(cron: &str, from_ms: i64) -> Option<i64> {
    let fields = parse_cron_expression(cron)?;
    compute_next_cron_run_ms(&fields, from_ms)
}

fn compute_next_cron_run_ms(fields: &CronFields, from_ms: i64) -> Option<i64> {
    let from =
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(from_ms)?.with_timezone(&Local);
    let mut cursor = from
        .naive_local()
        .with_second(0)?
        .with_nanosecond(0)?
        .checked_add_signed(chrono::Duration::minutes(1))?;

    let dom_wild = fields.day_of_month.len() == 31;
    let dow_wild = fields.day_of_week.len() == 7;

    for _ in 0..(366 * 24 * 60) {
        let month = cursor.month();
        let dom = cursor.day();
        let dow = cursor.weekday().num_days_from_sunday();
        let day_matches = if dom_wild && dow_wild {
            true
        } else if dom_wild {
            fields.day_of_week.contains(&dow)
        } else if dow_wild {
            fields.day_of_month.contains(&dom)
        } else {
            fields.day_of_month.contains(&dom) || fields.day_of_week.contains(&dow)
        };

        if fields.month.contains(&month)
            && day_matches
            && fields.hour.contains(&cursor.hour())
            && fields.minute.contains(&cursor.minute())
            && let Some(ts) = local_timestamp_millis(cursor)
        {
            return Some(ts);
        }

        cursor = cursor.checked_add_signed(chrono::Duration::minutes(1))?;
    }

    None
}

fn local_timestamp_millis(local: NaiveDateTime) -> Option<i64> {
    match Local.from_local_datetime(&local) {
        LocalResult::Single(dt) => Some(dt.timestamp_millis()),
        LocalResult::Ambiguous(dt, _) => Some(dt.timestamp_millis()),
        LocalResult::None => None,
    }
}

pub(crate) fn cron_to_human(cron: &str) -> String {
    let parts = cron.split_whitespace().collect::<Vec<_>>();
    if parts.len() != 5 {
        return cron.to_string();
    }
    let [minute, hour, day_of_month, month, day_of_week] = parts.as_slice() else {
        return cron.to_string();
    };

    if let Some(n) = minute
        .strip_prefix("*/")
        .and_then(|n| n.parse::<u32>().ok())
        && *hour == "*"
        && *day_of_month == "*"
        && *month == "*"
        && *day_of_week == "*"
    {
        return if n == 1 {
            "Every minute".to_string()
        } else {
            format!("Every {n} minutes")
        };
    }

    if minute.parse::<u32>().is_ok()
        && *hour == "*"
        && *day_of_month == "*"
        && *month == "*"
        && *day_of_week == "*"
    {
        let m = minute.parse::<u32>().unwrap_or(0);
        return if m == 0 {
            "Every hour".to_string()
        } else {
            format!("Every hour at :{m:02}")
        };
    }

    if minute.parse::<u32>().is_ok()
        && let Some(n) = hour.strip_prefix("*/").and_then(|n| n.parse::<u32>().ok())
        && *day_of_month == "*"
        && *month == "*"
        && *day_of_week == "*"
    {
        let m = minute.parse::<u32>().unwrap_or(0);
        let suffix = if m == 0 {
            String::new()
        } else {
            format!(" at :{m:02}")
        };
        return if n == 1 {
            format!("Every hour{suffix}")
        } else {
            format!("Every {n} hours{suffix}")
        };
    }

    if minute.parse::<u32>().is_err() || hour.parse::<u32>().is_err() {
        return cron.to_string();
    }
    let m = minute.parse::<u32>().unwrap_or(0);
    let h = hour.parse::<u32>().unwrap_or(0);

    if *day_of_month == "*" && *month == "*" && *day_of_week == "*" {
        return format!("Every day at {}", format_local_time(m, h));
    }

    if *day_of_month == "*" && *month == "*" && day_of_week.parse::<u32>().is_ok() {
        let day_names = [
            "Sunday",
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
        ];
        let index = day_of_week.parse::<usize>().unwrap_or(0) % 7;
        return format!("Every {} at {}", day_names[index], format_local_time(m, h));
    }

    if *day_of_month == "*" && *month == "*" && *day_of_week == "1-5" {
        return format!("Weekdays at {}", format_local_time(m, h));
    }

    cron.to_string()
}

fn format_local_time(minute: u32, hour: u32) -> String {
    let suffix = if hour < 12 { "AM" } else { "PM" };
    let hour12 = match hour % 12 {
        0 => 12,
        value => value,
    };
    format!("{hour12}:{minute:02} {suffix}")
}

const RECURRING_JITTER_FRAC: f64 = 0.1;
const RECURRING_JITTER_CAP_MS: i64 = 15 * 60 * 1000;
const ONE_SHOT_JITTER_MAX_MS: f64 = 90_000.0;
const ONE_SHOT_JITTER_FLOOR_MS: f64 = 0.0;
const ONE_SHOT_JITTER_MINUTE_MOD: u32 = 30;

pub(crate) fn jittered_next_cron_run_ms(cron: &str, from_ms: i64, task_id: &str) -> Option<i64> {
    let t1 = next_cron_run_ms(cron, from_ms)?;
    let Some(t2) = next_cron_run_ms(cron, t1) else {
        return Some(t1);
    };
    let jitter = (jitter_frac(task_id) * RECURRING_JITTER_FRAC * (t2 - t1) as f64)
        .min(RECURRING_JITTER_CAP_MS as f64) as i64;
    Some(t1 + jitter)
}

pub(crate) fn one_shot_jittered_next_cron_run_ms(
    cron: &str,
    from_ms: i64,
    task_id: &str,
) -> Option<i64> {
    let t1 = next_cron_run_ms(cron, from_ms)?;
    let minute = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(t1)?
        .with_timezone(&Local)
        .minute();
    if minute % ONE_SHOT_JITTER_MINUTE_MOD != 0 {
        return Some(t1);
    }
    let lead = ONE_SHOT_JITTER_FLOOR_MS
        + jitter_frac(task_id) * (ONE_SHOT_JITTER_MAX_MS - ONE_SHOT_JITTER_FLOOR_MS);
    Some((t1 - lead as i64).max(from_ms))
}

fn jitter_frac(task_id: &str) -> f64 {
    let prefix = task_id.chars().take(8).collect::<String>();
    u32::from_str_radix(&prefix, 16).map_or(0.0, |value| value as f64 / 0x1_0000_0000u64 as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_subset() {
        let fields = parse_cron_expression("*/5 9-17/2 * * 1-5").expect("valid cron");
        assert_eq!(fields.minute, (0..=55).step_by(5).collect::<Vec<_>>());
        assert_eq!(fields.hour, vec![9, 11, 13, 15, 17]);
        assert_eq!(fields.day_of_week, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn accepts_sunday_alias() {
        let fields = parse_cron_expression("0 9 * * 5-7").expect("valid cron");
        assert_eq!(fields.day_of_week, vec![0, 5, 6]);
    }

    #[test]
    fn rejects_unsupported_syntax() {
        assert!(parse_cron_expression("0 9 ? * MON").is_none());
        assert!(parse_cron_expression("0 9 * *").is_none());
        assert!(parse_cron_expression("0 24 * * *").is_none());
    }
}
