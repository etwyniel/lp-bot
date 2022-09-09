use anyhow::{anyhow, bail};
use chrono::{offset::FixedOffset, prelude::*, Duration};
use regex::Regex;
use serenity::{
    async_trait, model::prelude::interaction::application_command::ApplicationCommandInteraction,
    prelude::Context,
};
use serenity_command::CommandResponse;
use serenity_command_derive::Command;
use std::ops::Add;
use timezone_abbreviations::timezone;

use crate::{commands::BotCommand, Handler};

#[derive(Command)]
#[cmd(
    name = "relative",
    desc = "Convert a time of day to your local timezone",
    data = "Handler"
)]
pub struct Relative {
    #[cmd(desc = "Time of day with timezone, e.g. 7:30pm EST")]
    time: String,
}

#[async_trait]
impl BotCommand for Relative {
    async fn run(
        self,
        _handler: &Handler,
        _ctx: &Context,
        _opts: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let parsed = parse_time(&self.time)?;
        let contents = format!(
            "{} is at <t:{1}:t> (in <t:{1}:R>)",
            &self.time,
            parsed.timestamp()
        );
        Ok(CommandResponse::Public(contents))
    }
}

pub fn parse_time(time: &str) -> anyhow::Result<DateTime<FixedOffset>> {
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
    let tz = &tzs[0];
    let offset = FixedOffset::east((tz.hour_offset as i32 * 60 + tz.minute_offset as i32) * 60);
    let now = Utc::now();
    let mut now_tz = now.with_timezone(&offset);
    while now_tz.minute() != minute {
        now_tz = now_tz.add(Duration::minutes(1));
    }
    while now_tz.hour() != hour {
        now_tz = now_tz.add(Duration::hours(1));
    }
    Ok(now_tz)
}
