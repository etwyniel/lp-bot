use std::ops::Add;

use anyhow::bail;
use chrono::{prelude::*, Duration};
use regex::Regex;
use serde_json::Map;
use serenity::{builder::CreateThread, model::channel::ChannelType};

use crate::album::Album;
use crate::command_context::Responder;
use crate::{
    command_context::{SlashCommand, TextCommand},
    Handler,
};

fn convert_lp_time(time: Option<&str>) -> Result<String, anyhow::Error> {
    let time = match time {
        Some("now") | None => return Ok("now".to_string()),
        Some(t) => t,
    };
    let xx_re = Regex::new("(?i)(XX:?)?([0-5][0-9])")?;
    let plus_re = Regex::new(r"\+(([0-5])?[0-9])")?;
    let mut lp_time = Utc::now();
    if let Some(cap) = xx_re.captures(time) {
        let min: i64 = cap.get(2).unwrap().as_str().parse()?;
        if !(0..60).contains(&min) {
            bail!("Invalid time");
        }
        let cur_min = lp_time.minute() as i64;
        let to_add = if cur_min <= min {
            min - cur_min
        } else {
            (60 - cur_min) + min
        };
        lp_time = lp_time.add(Duration::minutes(to_add));
    } else if let Some(cap) = plus_re.captures(time) {
        let extra_mins: i64 = cap.get(1).unwrap().as_str().parse()?;
        lp_time = lp_time.add(Duration::minutes(extra_mins));
    } else {
        bail!("Invalid time {}", time);
    }

    Ok(format!("at <t:{0:}:t> (<t:{0:}:R>)", lp_time.timestamp()))
}

pub async fn run_lp<T: Responder>(
    responder: &T,
    guild_id: u64,
    mut lp_name: Option<String>,
    time: Option<String>,
    mut link: Option<String>,
    provider: Option<String>,
) -> anyhow::Result<()> {
    if lp_name.as_deref().map(|name| name.starts_with("https://")) == Some(true) {
        // As a special case for convenience, if we have a URL in lp_name, use that as link
        if link.is_some() {
            bail!("Too many links!");
        }
        link = lp_name.take();
    }
    let handler = responder.handler();
    let mut info = match (&lp_name, &link) {
        (Some(name), None) => handler.lookup_album(name, provider).await?,
        (None, Some(lnk)) => handler.get_album_info(lnk).await?,
        (None, None) => bail!("Please specify something to LP"),
        (Some(_), Some(_)) => None,
    }
    .unwrap_or_else(|| Album {
        name: lp_name,
        url: link,
        ..Default::default()
    });
    let when = convert_lp_time(time.as_deref())?;
    let role_id = handler.get_role_id(guild_id);
    let lp_name = match (&info.name, &info.artist) {
        (Some(name), Some(artist)) => format!("{} - {}", name, artist),
        (Some(name), None) => name.to_string(),
        _ => "this".to_string(),
    };
    if info.genres.is_empty() {
        if let Some(artist) = &info.artist {
            info.genres = responder.handler().lastfm.artist_top_tags(artist).await?;
        }
    }
    let mut resp_content = format!(
        "{} {} {}",
        role_id
            .map(|id| format!("<@&{}>", id))
            .unwrap_or("Listening party: ".to_string()),
        lp_name,
        when
    );
    if let Some(link) = info.url {
        resp_content.push_str("\n");
        resp_content.push_str(&link);
    }
    let message = responder.respond(&resp_content, role_id).await?;
    if handler.get_create_threads(guild_id) {
        // Create thread from response message
        let mut thread = CreateThread::default();
        thread
            .name(info.name.as_deref().unwrap_or("Listening party"))
            .kind(ChannelType::PublicThread)
            .auto_archive_duration(60);
        let map = Map::from_iter(thread.0.into_iter().map(|(k, v)| (k.to_string(), v)));
        responder
            .ctx()
            .http
            .create_public_thread(message.channel_id.0, message.id.0, &map)
            .await?;
    }
    Ok(())
}

impl Handler {
    pub fn get_create_threads(&self, guild_id: u64) -> bool {
        let db = self.db.lock().unwrap();
        db.query_row(
            "SELECT create_threads FROM guild WHERE id = ?1",
            [guild_id],
            |row| row.get(0),
        )
        .unwrap_or(false)
    }

    pub fn get_role_id(&self, guild_id: u64) -> Option<u64> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare("SELECT role_id FROM guild WHERE id = ?1").ok()?;
        stmt.query_row([guild_id], |row| row.get(0)).ok()
    }
}

impl TextCommand<'_, '_> {
    pub async fn run_lp(&self) -> anyhow::Result<()> {
        let mut msg: &str = &self.message.content;
        // Remove mentions from message
        let mut no_mentions = String::new();
        while !msg.is_empty() {
            let end = if let Some(ndx) = msg.find("<@") {
                ndx
            } else {
                msg.len()
            };
            no_mentions.push_str(&msg[..end]);
            msg = &msg[end..];
            if let Some(end) = msg.find(">") {
                msg = &msg[(end + 1)..];
            }
        }
        no_mentions = no_mentions.trim().to_string();

        // Extract time if present
        let mut lp_name = Some(no_mentions.clone());
        let mut time = None;
        let time_re = Regex::new(r"(?i)(.*?)\s+(at *)?XX:?([0-5]?[0-9])\s*$")?;
        if let Some(cap) = time_re.captures(&no_mentions) {
            lp_name = Some(cap.get(1).unwrap().as_str().to_string());
            time = Some(cap.get(3).unwrap().as_str().to_string())
        }

        run_lp(
            self,
            self.message.guild_id.unwrap().0,
            lp_name,
            time,
            None,
            None,
        )
        .await
    }
}

impl<'a, 'b> SlashCommand<'a, 'b> {
    pub async fn run_lp(
        &self,
        subject: Option<String>,
        time: Option<String>,
        link: Option<String>,
        provider: Option<String>,
    ) -> anyhow::Result<()> {
        run_lp(
            self,
            self.command.guild_id.unwrap().0,
            subject,
            link,
            time,
            provider,
        )
        .await
    }
}
