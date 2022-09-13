use std::collections::HashMap;
use std::env;
use std::fmt::Write;
use std::time::Duration;

use album::Album;
use anyhow::{anyhow, bail};
use autoreact::ReactsCache;
use chrono::{Datelike, Utc};
use lastfm::Lastfm;
use rusqlite::Connection;
use serenity::builder::CreateApplicationCommandOption;
use serenity::model::application::command::CommandType;
use serenity::model::application::interaction::autocomplete::AutocompleteInteraction;
use serenity::model::channel::Channel;
use serenity::model::event::ChannelPinsUpdateEvent;
use serenity::model::id::ChannelId;
use serenity::model::prelude::interaction::application_command::{
    ApplicationCommandInteraction, CommandDataOption, CommandDataOptionValue,
};
use serenity::model::prelude::Message;
use serenity::{
    async_trait,
    model::{
        application::command::{Command, CommandOptionType},
        application::interaction::{Interaction, InteractionResponseType, MessageFlags},
        gateway::GatewayIntents,
        gateway::Ready,
        id::GuildId,
    },
    prelude::*,
};
mod album;
mod autoreact;
mod bandcamp;
mod command_context;
mod commands;
mod db;
mod lastfm;
mod magik;
mod reltime;
mod spotify;

use album::AlbumProvider;
use bandcamp::Bandcamp;
use command_context::{get_focused_option, get_str_opt_ac, SlashCommand};
use db::Birthday;
use serenity_command::{BotCommand, CommandBuilder, CommandResponse, CommandRunner};
use spotify::Spotify;
use tokio::sync;

pub struct Handler {
    db: sync::Mutex<Connection>,
    providers: Vec<Box<dyn AlbumProvider>>,
    lastfm: Lastfm,
    reacts_cache: RwLock<ReactsCache>,
    commands: RwLock<HashMap<&'static str, Box<dyn CommandRunner<Handler> + Send + Sync>>>,
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

impl Handler {
    fn init_commands() -> HashMap<&'static str, Box<dyn CommandRunner<Handler> + Send + Sync>> {
        let mut commands = HashMap::new();
        commands::register_commands(&mut commands);
        commands
    }

    async fn new() -> anyhow::Result<Self> {
        let conn = db::init()?;
        let providers = vec![
            Box::new(Spotify::new().await?) as Box<dyn AlbumProvider>,
            Box::new(Bandcamp::new()),
        ];
        let lastfm = Lastfm::new();
        let reacts_cache = RwLock::new(autoreact::new(&conn).await?);
        let commands = RwLock::new(Self::init_commands());
        Ok(Handler {
            db: sync::Mutex::new(conn),
            providers,
            lastfm,
            reacts_cache,
            commands,
        })
    }

    pub fn get_provider(&self, provider: Option<&str>) -> &dyn AlbumProvider {
        provider
            .and_then(|id| self.providers.iter().find(|p| p.id() == id))
            .or_else(|| self.providers.first())
            .unwrap()
            .as_ref()
    }

    async fn get_album_info(&self, link: &str) -> anyhow::Result<Option<Album>> {
        if let Some(p) = self.providers.iter().find(|p| p.url_matches(link)) {
            let info = p.get_from_url(link).await?;
            return Ok(Some(info));
        }
        Ok(None)
    }

    pub async fn lookup_album(
        &self,
        query: &str,
        provider: Option<&str>,
    ) -> anyhow::Result<Option<Album>> {
        let p = self.get_provider(provider);
        p.query_album(query).await.map(Some)
    }

    pub async fn query_albums(
        &self,
        query: &str,
        provider: Option<&str>,
    ) -> anyhow::Result<Vec<(String, String)>> {
        let p = self.get_provider(provider);
        let mut choices = p.query_albums(query).await?;
        choices.iter_mut().for_each(|(name, _)| {
            if name.len() >= 100 {
                *name = name[..100].to_string();
            }
        });
        Ok(choices)
    }

    async fn process_command(&self, cmd: SlashCommand<'_, '_>) -> anyhow::Result<CommandResponse> {
        let guild_id = cmd
            .command
            .guild_id
            .ok_or_else(|| anyhow!("Must be run in a server"))?
            .0;
        let data = &cmd.command.data;
        match cmd.name() {
            "quote" => {
                if let Some((_, message)) = cmd.command.data.resolved.messages.iter().next() {
                    let quote_number = self.add_quote(guild_id, message).await?;
                    let link = message.id.link(message.channel_id, Some(GuildId(guild_id)));
                    let resp_text = match quote_number {
                        Some(n) => format!("Quote saved as #{}: {}", n, link),
                        None => "Quote already added".to_string(),
                    };
                    Ok(CommandResponse::Public(resp_text))
                } else {
                    commands::GetQuote::from(data)
                        .run(self, cmd.ctx, cmd.command)
                        .await
                }
            }
            "bdays" => {
                let mut bdays = self.get_bdays(guild_id).await?;
                let today = Utc::today();
                let current_day = today.day() as u8;
                let current_month = today.month() as u8;
                bdays.sort_unstable_by_key(|Birthday { day, mut month, .. }| {
                    if month < current_month || (month == current_month && *day < current_day) {
                        month += 12;
                    }
                    month as u64 * 31 + *day as u64
                });
                let res = bdays
                    .into_iter()
                    .map(|b| format!("`{:02}/{:02}` • <@{}>", b.day, b.month, b.user_id))
                    .collect::<Vec<_>>()
                    .join("\n");
                cmd.command
                    .create_interaction_response(&cmd.ctx.http, |resp| {
                        resp.interaction_response_data(|data| {
                            let header = if let Some(server) =
                                cmd.command.guild_id.and_then(|g| g.name(&cmd.ctx))
                            {
                                format!("Birthdays in {}", server)
                            } else {
                                "Birthdays".to_string()
                            };
                            data.embed(|embed| embed.author(|a| a.name(header)).description(res))
                        })
                    })
                    .await?;
                Ok(CommandResponse::None)
            }
            "magik" => {
                let url = cmd.str_opt("url").unwrap();
                let scale = cmd.number_opt("scale").unwrap_or(0.5);
                cmd.command
                    .create_interaction_response(&cmd.ctx.http, |resp| {
                        resp.interaction_response_data(|data| data.content("Processing image..."))
                    })
                    .await?;
                let magiked = magik::magik(&url, scale)?;
                cmd.command
                    .create_followup_message(&cmd.ctx.http, |msg| {
                        msg.add_file((magiked.as_slice(), "out.png"))
                    })
                    .await?;
                Ok(CommandResponse::None)
            }
            _ => {
                if let Some(runner) = self.commands.read().await.get(cmd.name()) {
                    runner.run(self, cmd.ctx, cmd.command).await
                } else {
                    bail!("Unknown command")
                }
            }
        }
    }

    async fn process_autocomplete(
        &self,
        ctx: &Context,
        ac: AutocompleteInteraction,
    ) -> anyhow::Result<()> {
        let mut choices: Vec<(String, String)> = vec![];
        let options = &ac.data.options;
        let guild_id = ac
            .guild_id
            .ok_or_else(|| anyhow!("Must be run in a server"))?
            .0;
        match ac.data.name.as_str() {
            "lp" => {
                choices = self.autocomplete_lp(options).await?;
            }
            "quote" => {
                let val = get_str_opt_ac(options, "number");
                if let Some(v) = val {
                    let quotes = self.list_quotes(guild_id, v).await?;
                    ac.create_autocomplete_response(&ctx.http, |r| {
                        quotes
                            .into_iter()
                            .map(|(num, quote)| (num, quote.chars().take(100).collect::<String>()))
                            .for_each(|(num, q)| {
                                r.add_int_choice(q, num as i64);
                            });
                        r
                    })
                    .await?;
                    return Ok(());
                }
            }
            "remove_autoreact" => {
                let trigger = get_str_opt_ac(options, "trigger").unwrap_or("");
                let emote = get_str_opt_ac(options, "emote").unwrap_or("");
                let res = self
                    .autocomplete_autoreact(guild_id, trigger, emote)
                    .await?;
                let focused = match get_focused_option(options) {
                    Some(f) => f,
                    None => return Ok(()),
                };
                choices.extend(
                    res.into_iter()
                        .map(|(trigger, emote)| if focused == "trigger" { trigger } else { emote })
                        .map(|v| (v.clone(), v)),
                );
            }
            _ => (),
        }
        ac.create_autocomplete_response(&ctx.http, |r| {
            choices.into_iter().for_each(|(name, value)| {
                r.add_string_choice(name, value);
            });
            r
        })
        .await
        .map_err(anyhow::Error::from)
    }

    async fn crabdown(&self, ctx: &Context, channel: ChannelId) -> anyhow::Result<()> {
        for i in 0..3 {
            channel
                .send_message(&ctx.http, |msg| msg.content("🦀".repeat(3 - i)))
                .await?;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        channel
            .send_message(&ctx.http, |msg| {
                msg.content("<a:CrabRave:988508208240922635>")
            })
            .await?;
        Ok(())
    }

    async fn move_pin_to_pinboard(
        &self,
        ctx: &Context,
        channel: ChannelId,
        guild_id: GuildId,
    ) -> anyhow::Result<()> {
        let pins = channel
            .pins(&ctx.http)
            .await
            .context("getting pinned messages")?;
        let last_pin = match pins.last() {
            Some(m) => m,
            _ => return Ok(()),
        };
        dbg!(&last_pin);
        let pinboard_webhook = match self.get_pinboard_webhook(guild_id.0).await {
            Some(w) => w,
            _ => return Ok(()),
        };
        let author = &last_pin.author;
        let name = last_pin
            .author_nick(&ctx.http)
            .await
            .unwrap_or_else(|| author.name.clone());
        let avatar = guild_id
            .member(&ctx.http, author)
            .await
            .ok()
            .and_then(|member| member.avatar)
            .filter(|av| av.starts_with("http"))
            .or_else(|| author.avatar_url())
            .filter(|av| av.starts_with("http"));
        let channel_name = channel
            .name(&ctx)
            .await
            .unwrap_or_else(|| "unknown-channel".to_string());
        let image = last_pin
            .attachments
            .iter()
            .find(|at| at.height.is_some())
            .map(|at| at.url.as_str());
        ctx.http
            .get_webhook_from_url(&pinboard_webhook)
            .await
            .context("getting webhook")?
            .execute(&ctx.http, true, |message| {
                let mut embeds = Vec::with_capacity(last_pin.embeds.len() + 1);
                if !last_pin.content.is_empty() || image.is_some() {
                    embeds.push(Embed::fake(|val| {
                        image.map(|url| val.image(url));
                        val.description(format!(
                            "{}\n\n[(Source)]({})",
                            last_pin.content,
                            last_pin.link()
                        ))
                        .footer(|footer| {
                            footer
                                .text(format!("Message pinned from #{} using LPBot", channel_name))
                        })
                        .timestamp(last_pin.timestamp)
                    }))
                }
                embeds.extend(
                    last_pin
                        .embeds
                        .iter()
                        .filter(|em| em.kind.as_deref() == Some("rich"))
                        .map(|em| {
                            Embed::fake(|val| {
                                em.title.as_ref().map(|title| val.title(title));
                                em.url.as_ref().map(|url| val.url(url));
                                em.author.as_ref().map(|author| {
                                    val.author(|at| {
                                        author.url.as_ref().map(|url| at.url(url));
                                        author.icon_url.as_ref().map(|url| at.icon_url(url));
                                        at.name(&author.name)
                                    })
                                });
                                em.colour.as_ref().map(|colour| val.colour(*colour));
                                em.description
                                    .as_ref()
                                    .map(|description| val.description(description));
                                em.fields.iter().for_each(|f| {
                                    val.field(&f.name, &f.value, f.inline);
                                });
                                em.footer.as_ref().map(|footer| {
                                    val.footer(|f| {
                                        footer.icon_url.as_ref().map(|url| f.icon_url(url));
                                        f.text(&footer.text)
                                    })
                                });
                                em.image.as_ref().map(|image| val.image(&image.url));
                                em.thumbnail
                                    .as_ref()
                                    .map(|thumbnail| val.thumbnail(&thumbnail.url));
                                em.timestamp
                                    .as_deref()
                                    .map(|timestamp| val.timestamp(timestamp));
                                val
                            })
                        }),
                );
                message.embeds(embeds);
                avatar.map(|av| message.avatar_url(av));
                message.username(name)
            })
            .await
            .context("calling pinboard webhook")?;
        last_pin
            .unpin(&ctx.http)
            .await
            .context("deleting pinned message")?;
        Ok(())
    }
}

fn format_options(opts: &[CommandDataOption]) -> String {
    let mut out = String::new();
    for (i, opt) in opts.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&opt.name);
        out.push_str(": ");
        match &opt.resolved {
            None => out.push_str("None"),
            Some(CommandDataOptionValue::String(s)) => write!(&mut out, "{s:?}").unwrap(),
            Some(val) => write!(&mut out, "{val:?}").unwrap(),
        }
    }
    out
}

#[async_trait]
impl EventHandler for Handler {
    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Autocomplete(ac) = interaction {
            self.process_autocomplete(&ctx, ac).await.unwrap();
        } else if let Interaction::ApplicationCommand(command) = interaction {
            // log command
            let guild_name = command
                .guild_id
                .and_then(|g| g.name(&ctx))
                .map(|name| format!("[{name}] "))
                .unwrap_or_default();
            let user = &command.user.name;
            let name = &command.data.name;
            let params = format_options(&command.data.options);
            eprintln!("{guild_name}{user}: /{name} {params}");

            let cmd = SlashCommand {
                handler: self,
                ctx: &ctx,
                command: &command,
            };
            let (contents, flags) = match self.process_command(cmd).await {
                Ok(CommandResponse::None) => return,
                Ok(CommandResponse::Public(s)) => (s, MessageFlags::empty()),
                Ok(CommandResponse::Private(s)) => (s, MessageFlags::EPHEMERAL),
                Err(e) => {
                    eprintln!("Error processing command {}: {:?}", &command.data.name, e);
                    (e.to_string(), MessageFlags::EPHEMERAL)
                }
            };

            if let Err(why) = command
                .create_interaction_response(&ctx.http, |response| {
                    response
                        .kind(InteractionResponseType::ChannelMessageWithSource)
                        .interaction_response_data(|message| message.content(contents).flags(flags))
                })
                .await
            {
                eprintln!("cannot respond to slash command: {}", why);
                return;
            }
        }
    }

    async fn reaction_add(&self, ctx: Context, add_reaction: serenity::model::channel::Reaction) {
        if !add_reaction.emoji.unicode_eq("🗨️") {
            return;
        }
        if let Some(id) = add_reaction.guild_id {
            let message = match add_reaction.message(&ctx.http).await {
                Ok(m) => m,
                Err(_) => return,
            };
            let number = match self.add_quote(id.0, &message).await {
                Ok(Some(n)) => n,
                Ok(None) => return,
                Err(e) => {
                    eprintln!("Error adding quote: {}", e);
                    return;
                }
            };
            if let Ok(Channel::Guild(g)) = add_reaction.channel(&ctx.http).await {
                g.send_message(&ctx.http, |m| {
                    m.reference_message((g.id, message.id))
                        .allowed_mentions(|mentions| mentions.empty_users())
                        .content(&format!("Quote saved as #{}", number))
                })
                .await
                .unwrap();
            }
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);

        let guild_id = GuildId(
            env::var("GUILD_ID")
                .expect("Expected GUILD_ID in environment")
                .parse()
                .expect("GUILD_ID must be an integer"),
        );

        let providers = self
            .providers
            .iter()
            .map(|p| p.id())
            .take(25)
            .collect::<Vec<_>>();
        GuildId::set_application_commands(&guild_id, &ctx.http, |commands| {
            commands
                .create_application_command(|command| reltime::Relative::create(command))
                .create_application_command(|command| commands::SetBday::runner().register(command))
                .create_application_command(|command| {
                    command.name("bdays").description("List server birthdays")
                })
                .create_application_command(|command| {
                    command
                        .name("magik")
                        .description("fuck up an image")
                        .create_option(|option| {
                            option
                                .name("url")
                                .description("Image URL")
                                .kind(CommandOptionType::String)
                                .required(true)
                        })
                        .create_option(|option| {
                            option
                                .name("scale")
                                .description("Scale")
                                .kind(CommandOptionType::Number)
                                .min_number_value(0.00001)
                                .max_number_value(4.)
                        })
                })
        })
        .await
        .unwrap();

        let provider_extra = |opt_name: &str, opt: &mut CreateApplicationCommandOption| {
            if opt_name == "provider" {
                providers.iter().for_each(|p| {
                    opt.add_string_choice(p, p);
                })
            }
        };
        Command::create_global_application_command(&ctx.http, |command| {
            commands::lp::Lp::create_extras(command, provider_extra)
        })
        .await
        .unwrap();
        Command::create_global_application_command(&ctx.http, |command| {
            commands::AlbumLookup::create_extras(command, provider_extra)
        })
        .await
        .unwrap();
        Command::create_global_application_command(&ctx.http, |command| {
            commands::SetLpRole::create(command)
                .default_member_permissions(Permissions::MANAGE_ROLES)
        })
        .await
        .unwrap();
        Command::create_global_application_command(&ctx.http, |command| {
            commands::SetCreateThreads::create(command)
                .default_member_permissions(Permissions::MANAGE_THREADS)
        })
        .await
        .unwrap();
        Command::create_global_application_command(&ctx.http, |command| {
            command.name("quote").kind(CommandType::Message)
        })
        .await
        .unwrap();
        Command::create_global_application_command(&ctx.http, |command| {
            commands::GetQuote::create_extras(
                command,
                |opt_name, opt: &mut CreateApplicationCommandOption| {
                    if opt_name == "number" {
                        opt.min_int_value(1);
                    }
                },
            )
        })
        .await
        .unwrap();
        Command::create_global_application_command(&ctx.http, |command| {
            commands::SetWebhook::create(command)
                .default_member_permissions(Permissions::MANAGE_WEBHOOKS)
        })
        .await
        .unwrap();
        Command::create_global_application_command(&ctx.http, |command| {
            commands::SetPinboardWebhook::create(command)
                .default_member_permissions(Permissions::MANAGE_WEBHOOKS)
        })
        .await
        .unwrap();
    }

    async fn message(&self, ctx: Context, new_message: Message) {
        if &new_message.content == ".fmcrabdown" {
            if let Err(e) = self.crabdown(&ctx, new_message.channel_id).await {
                eprintln!("Error sending countdown: {}", e);
            }
            return;
        }
        if let Err(e) = self.add_reacts(&ctx, new_message).await {
            eprintln!("Error adding reacts: {}", e);
        }
    }

    async fn channel_pins_update(&self, ctx: Context, pin: ChannelPinsUpdateEvent) {
        let guild_id = match pin.guild_id {
            Some(gid) => gid,
            None => return,
        };
        if let Err(e) = self
            .move_pin_to_pinboard(&ctx, pin.channel_id, guild_id)
            .await
        {
            eprintln!("Error moving message to pinboard: {}", e);
        }
    }
}

#[tokio::main]
async fn main() {
    let handler = match Handler::new().await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Initialization failed: {}", e);
            return;
        }
    };

    let token = env::var("DISCORD_TOKEN").expect("Expected a token in the environment");

    let application_id: u64 = env::var("APPLICATION_ID")
        .expect("Expected an application id in the environment")
        .parse()
        .expect("application id is not a valid id");

    // Build our client.
    let mut client = Client::builder(
        token,
        GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::GUILD_MESSAGE_REACTIONS
            | GatewayIntents::MESSAGE_CONTENT
            | GatewayIntents::GUILDS,
    )
    .event_handler(handler)
    .application_id(application_id)
    .await
    .expect("Error creating client");

    // Start a single shard, and start listening to events.
    //
    // Shards will automatically attempt to reconnect, and will perform
    // exponential backoff until it reconnects.
    if let Err(why) = client.start().await {
        println!("Client error: {:?}", why);
    }
}
