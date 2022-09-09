use serenity::model::prelude::interaction::application_command::ApplicationCommandInteraction;
use serenity::{
    async_trait,
    model::prelude::{interaction::InteractionResponseType, GuildId, Role, UserId},
    prelude::Context,
};
use serenity_command::{CommandBuilder, CommandResponse, CommandRunner};
use serenity_command_derive::Command;

use std::collections::HashMap;
use std::fmt::Write;

use crate::Handler;
use crate::reltime::Relative;

use anyhow::{anyhow, bail};

pub mod lp;

use lp::Lp;

#[async_trait]
pub trait BotCommand {
    async fn run(
        self,
        handler: &Handler,
        ctx: &Context,
        interaction: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse>;
}

trait InteractionExt {
    fn guild_id(&self) -> anyhow::Result<GuildId>;
}

impl InteractionExt for ApplicationCommandInteraction {
    fn guild_id(&self) -> anyhow::Result<GuildId> {
        self.guild_id
            .ok_or_else(|| anyhow!("Must be run in a server"))
    }
}

#[derive(Command)]
#[cmd(name = "album", desc = "lookup an album", data = "Handler")]
pub struct AlbumLookup {
    #[cmd(desc = "The album you are looking for (e.g. band - album)")]
    album: String,
    #[cmd(desc = "Where to look for album info (defaults to spotify)")]
    provider: Option<String>,
}

#[async_trait]
impl BotCommand for AlbumLookup {
    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        _opts: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let mut info = match handler
            .lookup_album(&self.album, self.provider.as_deref())
            .await?
        {
            None => bail!("Not found"),
            Some(info) => info,
        };
        let mut contents = format!(
            "{}{}\n",
            info.format_name(),
            info.release_date
                .as_deref()
                .map(|d| format!(" ({})", d))
                .unwrap_or_default(),
        );
        if info.genres.is_empty() {
            if let Some(artist) = &info.artist {
                info.genres = handler.lastfm.artist_top_tags(artist).await?;
            }
        }
        if let Some(genres) = info.format_genres() {
            _ = writeln!(&mut contents, "{}", &genres);
        }
        contents.push_str(info.url.as_deref().unwrap_or("no link found"));
        Ok(CommandResponse::Public(contents))
    }
}

#[derive(Command)]
#[cmd(
    name = "setrole",
    desc = "set what role to ping for listening parties",
    data = "Handler"
)]
pub struct SetLpRole {
    #[cmd(desc = "Role to ping (leave unset to clear)")]
    role: Option<Role>,
}

#[async_trait]
impl BotCommand for SetLpRole {
    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        opts: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        handler
            .set_guild_field(
                "role",
                opts.guild_id()?.0,
                self.role.as_ref().map(|r| r.id.0),
            )
            .await?;
        let contents = match self.role {
            Some(r) => format!("LP role changed to <@&{}>", r.id.0),
            None => "LP role removed".to_string(),
        };
        Ok(CommandResponse::Public(contents))
    }
}

#[derive(Command)]
#[cmd(
    name = "setcreatethreads",
    desc = "Configure whether LPBot should create threads for LPs",
    data = "Handler"
)]
pub struct SetCreateThreads {
    #[cmd(desc = "Create threads for LPs")]
    create_threads: Option<bool>,
}

#[async_trait]
impl BotCommand for SetCreateThreads {
    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        opts: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        handler
            .set_guild_field(
                "create_threads",
                opts.guild_id()?.0,
                self.create_threads.unwrap_or(false),
            )
            .await?;
        let contents = format!(
            "LPBot will {}create threads for listening parties",
            if self.create_threads == Some(true) {
                ""
            } else {
                "not "
            }
        );
        Ok(CommandResponse::Public(contents))
    }
}

#[derive(Command)]
#[cmd(name = "quote", desc = "Retrieve a quote", data = "Handler")]
pub struct GetQuote {
    #[cmd(desc = "Number the quote was saved as (optional)", autocomplete)]
    number: Option<i64>,
}

#[async_trait]
impl BotCommand for GetQuote {
    async fn run(
        self,
        handler: &Handler,
        ctx: &Context,
        opts: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let guild_id = opts.guild_id()?.0;
        let quote = if let Some(quote_number) = self.number {
            handler.fetch_quote(guild_id, quote_number as u64).await?
        } else {
            handler.get_random_quote(guild_id).await?
        }
        .ok_or_else(|| anyhow!("No such quote"))?;
        let message_url = format!(
            "https://discord.com/channels/{}/{}/{}",
            quote.guild_id, quote.channel_id, quote.message_id
        );
        let contents = format!(
            "{}\n - <@{}> [(Source)]({})",
            &quote.contents, quote.author_id, message_url
        );
        let author_avatar = UserId(quote.author_id)
            .to_user(&ctx.http)
            .await?
            .avatar_url()
            .filter(|av| av.starts_with("http"));
        opts.create_interaction_response(&ctx.http, |resp| {
            resp.kind(InteractionResponseType::ChannelMessageWithSource)
                .interaction_response_data(|data| {
                    data.embed(|embed| {
                        embed
                            .author(|a| {
                                author_avatar.map(|av| a.icon_url(av));
                                a.name(format!("#{}", quote.quote_number))
                            })
                            .description(&contents)
                            .url(message_url)
                            .timestamp(quote.ts.format("%+").to_string())
                    })
                })
        })
        .await?;
        Ok(CommandResponse::None)
    }
}

#[derive(Command)]
#[cmd(
    name = "setwebhook",
    desc = "Set (or unset) a webhook for LPBot to use when creating listening parties",
    data = "Handler"
)]
pub struct SetWebhook {
    #[cmd(desc = "The webhook URL (leave empty to remove)")]
    webhook: Option<String>,
}

#[async_trait]
impl BotCommand for SetWebhook {
    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        opts: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        handler
            .set_guild_field("webhook", opts.guild_id()?.0, self.webhook.as_deref())
            .await?;
        let contents = format!(
            "LPBot will {}use a webhook",
            if self.webhook.is_some() { "" } else { "not " }
        );
        Ok(CommandResponse::Private(contents))
    }
}

#[derive(Command)]
#[cmd(
    name = "setpinboardwebhook",
    desc = "Set (or unset) a webhook for the pinboard channel",
    data = "Handler"
)]
pub struct SetPinboardWebhook {
    #[cmd(desc = "The webhook URL for the pinboard channel (leave empty to remove)")]
    webhook: Option<String>,
}

#[async_trait]
impl BotCommand for SetPinboardWebhook {
    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        opts: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let guild_id = opts.guild_id()?;
        handler
            .set_guild_field("pinboard_webhook", guild_id.0, self.webhook.as_deref())
            .await?;
        Ok(CommandResponse::Private(
            if self.webhook.is_some() {
                "Pinboard webhook set"
            } else {
                "Pinboard webhook removed"
            }
            .to_string(),
        ))
    }
}

#[derive(Command)]
#[cmd(name = "bday", desc = "Set your birthday", data = "Handler")]
pub struct SetBday {
    #[cmd(desc = "Day")]
    day: i64,
    #[cmd(desc = "Month")]
    month: i64,
    #[cmd(desc = "Year")]
    year: Option<i64>,
}

#[async_trait]
impl BotCommand for SetBday {
    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        opts: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let user_id = opts.user.id.0;
        handler
            .add_birthday(
                opts.guild_id()?.0,
                user_id,
                self.day as u8,
                self.month as u8,
                self.year.map(|y| y as u16),
            )
            .await?;
        Ok(CommandResponse::Private("Birthday set!".to_string()))
    }
}

pub fn register_commands(commands: &mut HashMap<&'static str, Box< dyn CommandRunner<Handler> + Send + Sync>>) {
    let mut add = |runner: Box<dyn CommandRunner<Handler> + Send + Sync>| commands.insert(runner.name(), runner);
    add(Lp::runner());
    add(AlbumLookup::runner());
    add(SetLpRole::runner());
    add(SetCreateThreads::runner());
    add(SetWebhook::runner());
    add(SetPinboardWebhook::runner());
    add(SetBday::runner());
    add(Relative::runner());
}
