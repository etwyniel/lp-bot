use std::future::Future;
use std::ops::Add;

use anyhow::anyhow;
use chrono::{prelude::*, Duration};
use regex::Regex;
use rusqlite::params;
use serde_json::Map;
use serenity::{
    builder::CreateThread,
    model::{
        channel::{Channel, ChannelType, Message},
        interactions::{
            application_command::ApplicationCommandInteraction, InteractionResponseType,
        },
    },
    prelude::*,
};

use crate::Handler;

pub struct Command<'a, 'b> {
    pub handler: &'a Handler,
    pub ctx: &'b Context,
    pub command: &'b ApplicationCommandInteraction,
}

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
            return Err(anyhow!("Invalid time"));
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
        return Err(anyhow!("Invalid time {}", time));
    }

    Ok(format!("at <t:{0:}:t> (<t:{0:}:R>)", lp_time.timestamp()))
}

impl Handler {
    pub fn get_role_id(&self, guild_id: u64) -> Option<u64> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare("SELECT role_id FROM guild WHERE id = ?1").ok()?;
        stmt.query_row(params![guild_id], |row| row.get(0)).ok()
    }

    pub async fn run_lp<Fut: Future<Output = anyhow::Result<Message>>>(
        &self,
        ctx: &Context,
        guild_id: u64,
        mut lp_name: Option<String>,
        time: Option<String>,
        mut link: Option<String>,
        send_message: impl FnOnce(String, Option<u64>) -> Fut,
    ) -> anyhow::Result<()> {
        if lp_name.as_deref().map(|name| name.starts_with("https://")) == Some(true) {
            // As a special case for convenience, if we have a URL in lp_name, use that as link
            if link.is_some() {
                return Err(anyhow!("Too many links!"));
            }
            link = lp_name.take();
        }
        match (&lp_name, &link) {
            (Some(name), None) => match self.lookup_album(name).await? {
                Some((lnk, title)) => {
                    link = Some(lnk);
                    lp_name = Some(title);
                }
                _ => {}
            },
            (None, Some(lnk)) => lp_name = self.get_album_info(lnk).await?,
            (None, None) => return Err(anyhow!("Please specify something to LP")),
            _ => {}
        }
        let when = convert_lp_time(time.as_deref())?;
        let role_id = self.get_role_id(guild_id);
        let mut resp_content = format!(
            "{} {} {}",
            role_id
                .map(|id| format!("<@&{}>", id))
                .unwrap_or("Listening party: ".to_string()),
            lp_name.as_deref().unwrap_or("this"),
            when
        );
        if let Some(link) = link {
            resp_content.push_str("\n");
            resp_content.push_str(&link);
        }
        let message = send_message(resp_content, role_id).await?;
        // Create thread from response message
        let mut thread = CreateThread::default();
        thread
            .name(lp_name.as_deref().unwrap_or("Listening party"))
            .kind(ChannelType::PublicThread)
            .auto_archive_duration(60);
        let map = Map::from_iter(thread.0.into_iter().map(|(k, v)| (k.to_string(), v)));
        ctx.http
            .create_public_thread(message.channel_id.0, message.id.0, &map)
            .await?;
        Ok(())
    }

    pub async fn text_command_lp(&self, ctx: &Context, message: Message) -> anyhow::Result<()> {
        let mut msg: &str = &message.content;
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
            lp_name= Some(cap.get(1).unwrap().as_str().to_string());
            time = Some(cap.get(3).unwrap().as_str().to_string())
        }

        let channel = match message.channel(&ctx.cache).await {
            Some(Channel::Guild(c)) => c,
            _ => return Err(anyhow!("Invalid channel")),
        };
        let sender = |contents: String, role_id: Option<u64>| async move {
            channel.send_message(&ctx.http, |msg| msg
            .content(&contents)
            .allowed_mentions(|mentions| mentions.roles(role_id)))
            .await.map_err(anyhow::Error::from)
        };
        self.run_lp(&ctx, message.guild_id.unwrap().0, lp_name, time, None, sender).await
    }
}

impl<'a, 'b> Command<'a, 'b> {
    pub async fn run_lp(
        &self,
        subject: Option<String>,
        time: Option<String>,
        link: Option<String>,
    ) -> anyhow::Result<()> {
        let sender = |contents: String, role_id: Option<u64>| async move {
            self.command
                .create_interaction_response(&self.ctx.http, |response| {
                    response
                        .kind(InteractionResponseType::ChannelMessageWithSource)
                        .interaction_response_data(|message| {
                            message
                                .content(&contents)
                                .allowed_mentions(|m| m.roles(role_id))
                        })
                })
                .await?;
            self.command.get_interaction_response(&self.ctx.http).await.map_err(anyhow::Error::from)
        };
        self.handler.run_lp(self.ctx, self.command.guild_id.unwrap().0, subject, time, link, sender).await
    }

    pub async fn lp(
        &self,
        lp_name: Option<String>,
        link: Option<String>,
        time: Option<String>,
    ) -> anyhow::Result<()> {
        self.run_lp(lp_name, time, link).await
    }
}
