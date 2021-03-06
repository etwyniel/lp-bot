use std::borrow::Cow;
use std::fmt::Write;
use std::ops::Add;

use anyhow::bail;
use chrono::{prelude::*, Duration};
use regex::Regex;
use serenity::model::channel::ChannelType;
use serenity::model::interactions::application_command::ApplicationCommandInteractionDataOption;

use crate::album::Album;
use crate::command_context::{get_focused_option, get_str_opt_ac, Responder};
use crate::{command_context::SlashCommand, Handler};

fn convert_lp_time(time: Option<&str>) -> Result<String, anyhow::Error> {
    let time = match time {
        Some("now") | None => return Ok("now".to_string()),
        Some(t) => t,
    };
    let xx_re = Regex::new("(?i)^(XX:?)?([0-5][0-9])$")?; // e.g. XX:15, xx15 or 15
    let plus_re = Regex::new(r"\+(([0-5])?[0-9])")?; // e.g. +25
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

    // timestamp and relative time
    Ok(format!("at <t:{0:}:t> (<t:{0:}:R>)", lp_time.timestamp()))
}

async fn build_message_contents(
    handler: &Handler,
    info: &mut Album,
    time: Option<&str>,
    role_id: Option<u64>,
) -> anyhow::Result<String> {
    let when = convert_lp_time(time)?;
    let lp_name = info.format_name();
    if let (true, Some(artist)) = (info.genres.is_empty(), &info.artist) {
        // No genres, try to get some from last.fm
        match handler.lastfm.artist_top_tags(artist).await {
            Ok(genres) => info.genres = genres,
            Err(err) => {
                // Log error but carry on
                eprintln!("Couldn't retrieve genres from lastfm: {}", err);
            }
        }
    }
    let mut resp_content = format!(
        "{} {} {}\n",
        role_id // mention role if set
            .map(|id| format!("<@&{}>", id))
            .unwrap_or_else(|| "Listening party: ".to_string()),
        lp_name,
        when
    );
    if let Some(genres) = info.format_genres() {
        _ = writeln!(&mut resp_content, "{}", &genres);
    }
    if let Some(link) = &info.url {
        _ = writeln!(&mut resp_content, "{}", &link);
    }
    Ok(resp_content)
}

impl<'a, 'b> SlashCommand<'a, 'b> {
    pub async fn run_lp(
        &self,
        mut lp_name: Option<String>,
        mut link: Option<String>,
        time: Option<String>,
        provider: Option<String>,
    ) -> anyhow::Result<()> {
        if lp_name.as_deref().map(|name| name.starts_with("https://")) == Some(true) {
            // As a special case for convenience, if we have a URL in lp_name, use that as link
            if link.is_some() && link != lp_name {
                bail!("Too many links!");
            }
            link = lp_name.take();
        }
        let handler = self.handler;
        let http = &self.ctx.http;
        // Depending on what we have, look up more information
        let mut info = match (&lp_name, &link) {
            (Some(name), None) => handler.lookup_album(name, provider.as_deref()).await?,
            (name, Some(lnk)) => {
                let mut info = handler.get_album_info(lnk).await?;
                if let Some((info, name)) = info.as_mut().zip(name.clone()) {
                    info.name = Some(name)
                };
                info
            }
            (None, None) => bail!("Please specify something to LP"),
        }
        .unwrap_or_else(|| Album {
            name: lp_name,
            url: link,
            ..Default::default()
        });

        let guild_id = self.command.guild_id.unwrap().0;
        let role_id = handler.get_role_id(guild_id);
        let resp_content =
            build_message_contents(handler, &mut info, time.as_deref(), role_id).await?;
        let webhook = handler.get_webhook(guild_id);
        let message = if let Some(url) = webhook.as_deref() {
            // Send LP message through webhook
            // This lets us impersonate the user who sent the command
            let wh = http.get_webhook_from_url(url).await?;
            let user = &self.command.user;
            let avatar_url = user.avatar_url();
            let nick = user // try to get the user's nickname
                .nick_in(http, guild_id)
                .await
                .map(Cow::Owned)
                .unwrap_or_else(|| Cow::Borrowed(&user.name));
            wh.execute(http, true, |msg| {
                msg.content(&resp_content)
                    .allowed_mentions(|mentions| mentions.roles(role_id))
                    .username(nick);
                avatar_url.map(|url| msg.avatar_url(url));
                msg
            })
            .await?
            .unwrap() // Message is present because we set wait to true in execute
        } else {
            // Create interaction response
            self.respond(&resp_content, role_id).await?
        };
        let mut response = format!(
            "LP created: {}",
            message.id.link(message.channel_id, self.command.guild_id)
        );
        if handler.get_create_threads(guild_id) {
            // Create a thread from the response message for the LP to take place in
            let chan = message.channel(http).await?;
            let thread_name = info.name.as_deref().unwrap_or("Listening party");
            let guild_chan = chan.guild().map(|c| (c.kind, c));
            if let (None, Some((ChannelType::PublicThread, c))) = (&webhook, &guild_chan) {
                // If we're already in a thread, just rename it
                // unless we are using a webhook, in which case we can create a new thread
                c.edit_thread(http, |t| t.name(&thread_name)).await?;
            } else if let Some((ChannelType::Text, c)) = &guild_chan {
                // Create thread from response message
                let thread = c
                    .create_public_thread(http, message.id, |thread| {
                        thread
                            .name(thread_name)
                            .kind(ChannelType::PublicThread)
                            .auto_archive_duration(60)
                    })
                    .await?;
                response = format!("LP created: <#{}>", thread.id.as_u64());
            }
        }
        if webhook.is_some() {
            // If we used a webhook, we still need to create the interaction response
            self.respond(&response, None).await?;
        }
        Ok(())
    }
}

impl Handler {
    pub async fn autocomplete_lp(
        &self,
        options: &[ApplicationCommandInteractionDataOption],
    ) -> anyhow::Result<Vec<(String, String)>> {
        let mut choices = vec![];
        let mut provider = get_str_opt_ac(options, "provider");
        let focused = get_focused_option(options);
        let mut album = get_str_opt_ac(options, "album");
        if let (Some(mut s), Some("album")) = (&mut album, focused) {
            if s.len() >= 7 && !s.starts_with("https://") {
                // if url, don't complete
                if let (None, Some(stripped)) = (&provider, s.strip_prefix("bc:")) {
                    // as a shorthand, search bandcamp for values with the prefix "bc:"
                    s = stripped;
                    provider = Some("bandcamp");
                }
                choices = self.query_albums(s, provider).await.unwrap_or_default();
            }
            if !s.is_empty() {
                choices.push((s.to_string(), s.to_string()));
            }
        } else if let (Some("link"), Some(album)) = (focused, &album) {
            // If album contains a url, suggest using the same url for link
            if album.starts_with("https://") {
                choices.push((album.to_string(), album.to_string()));
            }
        }
        Ok(choices)
    }
}
