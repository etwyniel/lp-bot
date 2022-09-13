use anyhow::{anyhow, bail};
use chrono::{offset::FixedOffset, prelude::*, Duration};
use regex::Regex;
use serenity::{
    async_trait, model::prelude::interaction::application_command::ApplicationCommandInteraction,
    prelude::Context,
};
use serenity_command::{BotCommand, CommandResponse};
use serenity_command_derive::Command;
use std::{fmt::Write, ops::Add};
use timezone_abbreviations::timezone;

use crate::Handler;

#[derive(Command)]
#[cmd(
    name = "relative",
    desc = "Convert a time of day to your local timezone"
)]
pub struct Relative {
    #[cmd(desc = "Time of day with timezone, e.g. 7:30pm EST")]
    time: String,
}

fn format_ts(ts: &DateTime<FixedOffset>) -> String {
    format!("<t:{0}:t> (in <t:{0}:R>)", ts.timestamp())
}

#[async_trait]
impl BotCommand for Relative {
    type Data = Handler;
    async fn run(
        self,
        _handler: &Handler,
        _ctx: &Context,
        _opts: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let parsed = parse_time(&self.time)?;

        let contents = if parsed.len() == 1 {
            format!("{} is at {}", &self.time, format_ts(&parsed[0].1),)
        } else {
            let mut out = format!("{} is:", self.time.as_str());
            for (name, time) in parsed.into_iter() {
                write!(&mut out, "\n- {}: {}", name, format_ts(&time)).unwrap();
            }
            out
        };
        Ok(CommandResponse::Public(contents))
    }
}

pub fn parse_time(time: &str) -> anyhow::Result<Vec<(String, DateTime<FixedOffset>)>> {
    let parse_time_re =
        Regex::new("(?i)([0-2]?[0-9])(?:[h: ]?([0-5]?[0-9]))? *(am|pm)? *(\\w+)").unwrap();
    let cap = parse_time_re
        .captures(time)
        .ok_or_else(|| anyhow!("Invalid time {}", time))?;
    let get = |i| cap.get(i).map(|c| c.as_str());

    let mut hour: u32 = get(1).unwrap().parse()?;
    let minute: u32 = get(2).unwrap_or("0").parse()?;
    let ampm = get(3);
    let tz_abbr = get(4).unwrap();

    if ampm.is_some() && (hour > 12 || hour == 0) || ampm.is_none() && hour >= 24 {
        bail!("Invalid time {}", time);
    }

    match ampm {
        Some("am") if hour == 12 => hour = 0,
        Some("pm") if hour != 12 => hour += 12,
        _ => (),
    }
    let tzs = timezone(&tz_abbr.to_uppercase()).ok_or_else(|| anyhow!("Invalid timezone"))?;
    Ok(tzs
        .iter()
        .map(|tz| {
            dbg!(tz);
            let offset_seconds = (tz.hour_offset as i32 * 60 + tz.minute_offset as i32) * 60;
            let offset = if tz.sign.is_minus() {
                FixedOffset::west(offset_seconds)
            } else {
                FixedOffset::east(offset_seconds)
            };
            let now = Utc::now();
            let mut now_tz = now.with_timezone(&offset);
            while now_tz.minute() != minute {
                now_tz = now_tz.add(Duration::minutes(1));
            }
            while now_tz.hour() != hour {
                now_tz = now_tz.add(Duration::hours(1));
            }
            (tz.name.to_owned(), now_tz)
        })
        .collect())
}
