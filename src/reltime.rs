use chrono::{offset::FixedOffset, prelude::*, Duration};
use std::ops::Add;
use timezone_abbreviations::timezone;

pub fn parse_time(time: &str) -> DateTime<FixedOffset> {
    let (time, abbr) = time
        .rsplit_once(' ')
        .expect("Invalid time, missing timezone");
    let tzs = timezone(abbr).expect("Invalid timezone");
    let tz = &tzs[0];
    let offset = FixedOffset::east((tz.hour_offset as i32 * 60 + tz.minute_offset as i32) * 60);
    let now = Utc::now();
    let mut now_tz = now.with_timezone(&offset);
    let (hour, mut min) = time.split_once(':').expect("Invalid time format");
    let mut hour = hour.parse::<u32>().unwrap();
    if min.ends_with("am") {
        if hour == 12 {
            hour = 0;
        }
        min = min.trim_end_matches("am");
    } else if min.ends_with("pm") {
        if hour != 12 {
            hour += 12;
        }
        min = min.trim_end_matches("pm");
    }
    let min = min.parse::<u32>().unwrap();
    while now_tz.minute() != min {
        now_tz = now_tz.add(Duration::minutes(1));
    }
    while now_tz.hour() != hour {
        now_tz = now_tz.add(Duration::hours(1));
    }
    now_tz
}
