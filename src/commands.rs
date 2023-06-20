use regex::Regex;
use serenity::builder::{CreateApplicationCommandOption, CreateEmbed};
use serenity::model::prelude::interaction::application_command::ApplicationCommandInteraction;
use serenity::model::prelude::ChannelId;
use serenity::model::Permissions;
use serenity::{
    async_trait,
    model::prelude::{GuildId, Role, UserId},
    prelude::Context,
};
use serenity_command::{BotCommand, CommandBuilder, CommandKey, CommandResponse, CommandRunner};
use serenity_command_derive::Command;

use std::collections::HashMap;
use std::fmt::Write;

use crate::lastfm::{GetAotys, GetSotys};
use crate::reltime::Relative;
use crate::Handler;

use anyhow::{anyhow, bail};

pub mod lp;
pub mod ready_poll;

use lp::Lp;

use self::ready_poll::ReadyPoll;

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
#[cmd(name = "album", desc = "lookup an album")]
pub struct AlbumLookup {
    #[cmd(desc = "The album you are looking for (e.g. band - album)")]
    album: String,
    #[cmd(desc = "Where to look for album info (defaults to spotify)")]
    provider: Option<String>,
}

#[async_trait]
impl BotCommand for AlbumLookup {
    type Data = Handler;
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
                .map(|d| format!(" ({d})"))
                .unwrap_or_default(),
        );
        if info.genres.is_empty() {
            if let Some(artist) = &info.artist {
                info.genres = handler.lastfm.artist_top_tags(artist).await?;
            }
        }
        if let Some(genres) = info.format_genres() {
            _ = writeln!(&mut contents, "{genres}");
        }
        contents.push_str(info.url.as_deref().unwrap_or("no link found"));
        Ok(CommandResponse::Public(contents))
    }
}

#[derive(Command)]
#[cmd(name = "setrole", desc = "set what role to ping for listening parties")]
pub struct SetLpRole {
    #[cmd(desc = "Role to ping (leave unset to clear)")]
    role: Option<Role>,
}

#[async_trait]
impl BotCommand for SetLpRole {
    type Data = Handler;
    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        opts: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        handler
            .set_guild_field(
                "role_id",
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

    const PERMISSIONS: Permissions = Permissions::MANAGE_ROLES;
}

#[derive(Command)]
#[cmd(
    name = "setcreatethreads",
    desc = "Configure whether LPBot should create threads for LPs"
)]
pub struct SetCreateThreads {
    #[cmd(desc = "Create threads for LPs")]
    create_threads: Option<bool>,
}

#[async_trait]
impl BotCommand for SetCreateThreads {
    type Data = Handler;
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
        Ok(CommandResponse::Private(contents))
    }

    const PERMISSIONS: Permissions = Permissions::MANAGE_THREADS;
}

#[derive(Command)]
#[cmd(name = "quote", desc = "Retrieve a quote")]
pub struct GetQuote {
    #[cmd(desc = "Number the quote was saved as (optional)", autocomplete)]
    pub number: Option<i64>,
    #[cmd(desc = "Get a random quote from a specific user")]
    pub user: Option<UserId>,
    #[cmd(desc = "Hide the username for even more confusion")]
    pub hide_author: Option<bool>,
}

#[async_trait]
impl BotCommand for GetQuote {
    type Data = Handler;
    async fn run(
        self,
        handler: &Handler,
        ctx: &Context,
        opts: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let guild_id = opts.guild_id()?.0;
        self.get_quote(handler, ctx, guild_id).await
    }

    fn setup_options(opt_name: &'static str, opt: &mut CreateApplicationCommandOption) {
        if opt_name == "number" {
            opt.min_int_value(1);
        }
    }
}

impl GetQuote {
    pub async fn get_quote(
        self,
        handler: &Handler,
        ctx: &Context,
        guild_id: u64,
    ) -> anyhow::Result<CommandResponse> {
        let quote = if let Some(quote_number) = self.number {
            handler.fetch_quote(guild_id, quote_number as u64).await?
        } else {
            handler
                .get_random_quote(guild_id, self.user.map(|u| u.0))
                .await?
        }
        .ok_or_else(|| anyhow!("No such quote"))?;
        let message_url = format!(
            "https://discord.com/channels/{}/{}/{}",
            quote.guild_id, quote.channel_id, quote.message_id
        );
        let channel = ChannelId(quote.channel_id)
            .to_channel(&ctx.http)
            .await?
            .guild();
        let channel_name = channel
            .as_ref()
            .map(|c| c.name())
            .unwrap_or("unknown-channel");
        let hide_author = self.hide_author == Some(true);
        let mut contents = format!(
            "{}\n - <@{}> [(Source)]({})",
            &quote.contents, quote.author_id, message_url
        );
        let author_avatar = if hide_author {
            None
        } else {
            UserId(quote.author_id)
                .to_user(&ctx.http)
                .await?
                .avatar_url()
                .filter(|av| av.starts_with("http"))
        };
        let quote_header = match (self.user, self.number, hide_author) {
            (_, Some(_), _) => "".to_string(), // Set quote number, not random
            (Some(_), _, false) => format!(" - Random quote from {}", &quote.author_name),
            (Some(_), _, true) => " - Random quote from REDACTED".to_string(),
            (None, None, _) => " - Random quote".to_string(),
        };
        if hide_author {
            let hide_author_re = Regex::new("(<@\\d+>)").unwrap();
            contents = hide_author_re.replace_all(&contents, "||$1||").to_string();
        }
        let mut create = CreateEmbed::default();
        create
            .author(|a| {
                author_avatar.map(|av| a.icon_url(av));
                a.name(format!("#{}{}", quote.quote_number, quote_header))
            })
            .description(&contents)
            .url(message_url)
            .footer(|f| f.text(format!("in #{channel_name}")))
            .timestamp(quote.ts.format("%+").to_string());
        if let Some(image) = quote.image {
            create.image(image);
        }
        Ok(CommandResponse::Embed(create))
    }
}

#[derive(Command)]
#[cmd(name = "fake_quote", desc = "Get a procedurally generated quote")]
pub struct FakeQuote {
    user: Option<UserId>,
    start: Option<String>,
}

#[async_trait]
impl BotCommand for FakeQuote {
    type Data = Handler;
    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        opts: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let chain = handler
            .quotes_markov_chain(
                opts.guild_id
                    .ok_or_else(|| anyhow!("must be run in a guild"))?
                    .0,
                self.user.map(|u| u.0),
            )
            .await?;
        let mut resp = if let Some(start) = self.start {
            chain.generate_str_from_token(&start)
        } else {
            chain.generate_str()
        };
        if resp.is_empty() {
            resp = "Failed to generate quote".to_string();
        }
        Ok(CommandResponse::Public(resp))
    }
}

#[derive(Command)]
#[cmd(
    name = "setwebhook",
    desc = "Set (or unset) a webhook for LPBot to use when creating listening parties"
)]
pub struct SetWebhook {
    #[cmd(desc = "The webhook URL (leave empty to remove)")]
    webhook: Option<String>,
}

#[async_trait]
impl BotCommand for SetWebhook {
    type Data = Handler;
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

    const PERMISSIONS: Permissions = Permissions::MANAGE_WEBHOOKS;
}

#[derive(Command)]
#[cmd(
    name = "setpinboardwebhook",
    desc = "Set (or unset) a webhook for the pinboard channel"
)]
pub struct SetPinboardWebhook {
    #[cmd(desc = "The webhook URL for the pinboard channel (leave empty to remove)")]
    webhook: Option<String>,
}

#[async_trait]
impl BotCommand for SetPinboardWebhook {
    type Data = Handler;
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

    const PERMISSIONS: Permissions = Permissions::MANAGE_WEBHOOKS;
}

#[derive(Command)]
#[cmd(name = "bday", desc = "Set your birthday")]
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
    type Data = Handler;
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

    fn setup_options(opt_name: &'static str, opt: &mut CreateApplicationCommandOption) {
        match opt_name {
            "day" => {
                opt.min_int_value(1).max_int_value(31);
            }
            "month" => {
                const MONTHS: [&str; 12] = [
                    "January",
                    "February",
                    "March",
                    "April",
                    "May",
                    "June",
                    "July",
                    "August",
                    "September",
                    "October",
                    "November",
                    "December",
                ];
                MONTHS.iter().enumerate().for_each(|(n, month)| {
                    opt.add_int_choice(month, n as i32 + 1);
                });
            }
            _ => {}
        }
    }
}

#[derive(Command)]
#[cmd(
    name = "add_autoreact",
    desc = "Automatically add reactions to messages"
)]
pub struct AddAutoreact {
    #[cmd(desc = "The word that will trigger the reaction (case-insensitive)")]
    trigger: String,
    #[cmd(desc = "The emote to react with")]
    emote: String,
}

#[async_trait]
impl BotCommand for AddAutoreact {
    type Data = Handler;
    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        opts: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        handler
            .add_autoreact(
                opts.guild_id()?.0,
                &self.trigger.to_lowercase(),
                &self.emote,
            )
            .await?;
        Ok(CommandResponse::Private("Autoreact added".to_string()))
    }

    const PERMISSIONS: Permissions = Permissions::MANAGE_EMOJIS_AND_STICKERS;
}

#[derive(Command)]
#[cmd(name = "remove_autoreact", desc = "Remove automatic reaction")]
pub struct RemoveAutoreact {
    #[cmd(
        desc = "The word that triggers the reaction (case-insensitive)",
        autocomplete
    )]
    trigger: String,
    #[cmd(desc = "The emote to stop reacting with", autocomplete)]
    emote: String,
}

#[async_trait]
impl BotCommand for RemoveAutoreact {
    type Data = Handler;
    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        opts: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        handler
            .remove_autoreact(
                opts.guild_id()?.0,
                &self.trigger.to_lowercase(),
                &self.emote,
            )
            .await?;
        Ok(CommandResponse::Private("Autoreact removed".to_string()))
    }

    const PERMISSIONS: Permissions = Permissions::MANAGE_EMOJIS_AND_STICKERS;
}

pub fn register_commands(
    commands: &mut HashMap<CommandKey<'static>, Box<dyn CommandRunner<Handler> + Send + Sync>>,
) {
    let mut add = |runner: Box<dyn CommandRunner<Handler> + Send + Sync>| {
        commands.insert(runner.name(), runner)
    };
    add(Lp::runner());
    add(AlbumLookup::runner());
    add(SetLpRole::runner());
    add(SetCreateThreads::runner());
    add(SetWebhook::runner());
    add(SetPinboardWebhook::runner());
    add(SetBday::runner());
    add(Relative::runner());
    add(AddAutoreact::runner());
    add(RemoveAutoreact::runner());
    add(GetAotys::runner());
    add(GetSotys::runner());
    add(ReadyPoll::runner());
    add(FakeQuote::runner());
}
