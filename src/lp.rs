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
        channel::{ChannelType, Message},
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
    let xx_re = Regex::new("([Xx]{2}:?)?([0-5][0-9])")?;
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
        subject: Option<&str>,
        time: Option<&str>,
        link: Option<&str>,
        send_message: impl FnOnce(String, Option<u64>) -> Fut,
    ) -> anyhow::Result<()> {
        let when = convert_lp_time(time)?;
        let role_id = self.get_role_id(guild_id);
        let mut resp_content = format!(
            "{} {} {}",
            role_id
                .map(|id| format!("<@&{}>", id))
                .unwrap_or("Listening party: ".to_string()),
            subject.unwrap_or("this"),
            when
        );
        if let Some(link) = link {
            resp_content.push_str("\n");
            resp_content.push_str(link);
        }
        let message = send_message(resp_content, role_id).await?;
        // Create thread from response message
        let mut thread = CreateThread::default();
        thread
            .name(subject.unwrap_or("Listening party"))
            .kind(ChannelType::PublicThread)
            .auto_archive_duration(60);
        let map = Map::from_iter(thread.0.into_iter().map(|(k, v)| (k.to_string(), v)));
        ctx.http
            .create_public_thread(message.channel_id.0, message.id.0, &map)
            .await?;
        Ok(())
    }
}

impl<'a, 'b> Command<'a, 'b> {
    fn get_role_id(&self) -> Option<u64> {
        let guild_id = self.command.guild_id?;
        let db = self.handler.db.lock().ok()?;
        let mut stmt = db.prepare("SELECT role_id FROM guild WHERE id = ?1").ok()?;
        stmt.query_row(params![guild_id.0], |row| row.get(0)).ok()
    }

    pub async fn run_lp(
        &self,
        subject: Option<&str>,
        time: Option<&str>,
        link: Option<&str>,
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
        return self.handler.run_lp(self.ctx, self.command.guild_id.unwrap().0, subject, time, link, sender).await;
        let when = convert_lp_time(time)?;
        let role_id = self.get_role_id();
        let mut resp_content = format!(
            "{} {} {}",
            role_id
                .map(|id| format!("<@&{}>", id))
                .unwrap_or("Listening party: ".to_string()),
            subject.unwrap_or("this"),
            when
        );
        if let Some(link) = link {
            resp_content.push_str("\n");
            resp_content.push_str(link);
        }
        if let Err(why) = self
            .command
            .create_interaction_response(&self.ctx.http, |response| {
                response
                    .kind(InteractionResponseType::ChannelMessageWithSource)
                    .interaction_response_data(|message| {
                        message
                            .content(&resp_content)
                            .allowed_mentions(|m| m.roles(role_id))
                    })
            })
            .await
        {
            eprintln!("cannot respond to slash command: {}", why);
            return Ok(());
        }
        // Create thread from interaction response message
        let message = self
            .command
            .get_interaction_response(&self.ctx.http)
            .await
            .unwrap();
        let mut thread = CreateThread::default();
        thread
            .name(subject.unwrap_or("Listening party"))
            .kind(ChannelType::PublicThread)
            .auto_archive_duration(60);
        let map = Map::from_iter(thread.0.into_iter().map(|(k, v)| (k.to_string(), v)));
        self.ctx
            .http
            .create_public_thread(self.command.channel_id.0, message.id.0, &map)
            .await?;
        Ok(())
    }

    pub async fn lp(
        &self,
        mut lp_name: Option<String>,
        mut link: Option<String>,
        time: Option<String>,
    ) -> anyhow::Result<()> {
        if lp_name.as_deref().map(|name| name.starts_with("https://")) == Some(true) {
            // As a special case for convenience, if we have a URL in lp_name, use that as link
            if link.is_some() {
                return Err(anyhow!("Too many links!"));
            }
            link = lp_name.take();
        }
        match (&lp_name, &link) {
            (Some(name), None) => match self.handler.lookup_album(name).await? {
                Some((lnk, title)) => {
                    link = Some(lnk);
                    lp_name = Some(title);
                }
                _ => {}
            },
            (None, Some(lnk)) => lp_name = self.handler.get_album_info(lnk).await?,
            (None, None) => return Err(anyhow!("Please specify something to LP")),
            _ => {}
        }
        self.run_lp(lp_name.as_deref(), time.as_deref(), link.as_deref())
            .await
    }
}
