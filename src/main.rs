use std::fmt::Write;
use std::{env, sync};

use album::Album;
use anyhow::{anyhow, bail};
use lastfm::Lastfm;
use rusqlite::Connection;
use serenity::futures::TryFutureExt;
use serenity::model::channel::{Channel, ChannelType, ReactionType};
use serenity::model::interactions::application_command::ApplicationCommandType;
use serenity::model::interactions::autocomplete::AutocompleteInteraction;
use serenity::{
    async_trait,
    model::{
        channel::Message,
        gateway::GatewayIntents,
        gateway::Ready,
        id::GuildId,
        interactions::{
            application_command::{ApplicationCommand, ApplicationCommandOptionType},
            Interaction, InteractionApplicationCommandCallbackDataFlags, InteractionResponseType,
        },
        permissions::Permissions,
    },
    prelude::*,
};
mod album;
mod bandcamp;
mod command_context;
mod db;
mod lastfm;
mod lp;
mod reltime;
mod spotify;

use album::AlbumProvider;
use bandcamp::Bandcamp;
use command_context::SlashCommand;
use spotify::Spotify;

pub struct Handler {
    db: sync::Mutex<Connection>,
    providers: Vec<Box<dyn AlbumProvider>>,
    lastfm: Lastfm,
}

impl Handler {
    async fn new() -> anyhow::Result<Self> {
        let conn = db::init()?;
        let providers = vec![
            Box::new(Spotify::new().await?) as Box<dyn AlbumProvider>,
            Box::new(Bandcamp::new()),
        ];
        let lastfm = Lastfm::new();
        Ok(Handler {
            db: sync::Mutex::new(conn),
            providers,
            lastfm,
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

    fn set_role(&self, guild_id: Option<u64>, role_id: Option<u64>) -> anyhow::Result<()> {
        let guild_id = match guild_id {
            Some(id) => id,
            None => bail!("Must be run in a server"),
        };
        self.set_guild_field("role_id", guild_id, role_id)
    }

    fn set_should_create_threads(&self, guild_id: Option<u64>, create: bool) -> anyhow::Result<()> {
        let guild_id = match guild_id {
            Some(id) => id,
            None => bail!("Must be run in a server"),
        };
        self.set_guild_field("create_threads", guild_id, create)
    }

    fn set_webhook(&self, guild_id: Option<u64>, webhook: Option<&str>) -> anyhow::Result<()> {
        let guild_id = match guild_id {
            Some(id) => id,
            None => bail!("Must be run in a server"),
        };
        self.set_guild_field("webhook", guild_id, webhook)
    }

    async fn process_command(&self, cmd: SlashCommand<'_, '_>) -> anyhow::Result<Option<String>> {
        let guild_id = cmd
            .command
            .guild_id
            .ok_or_else(|| anyhow!("Must be run in a server"))?;
        match cmd.name() {
            "lp" => {
                let lp_name = cmd.str_opt("album");
                let time = cmd.str_opt("time");
                let link = cmd.str_opt("link");
                let provider = cmd.str_opt("provider");
                cmd.run_lp(lp_name, link, time, provider)
                    .await
                    .map(|_| None)
            }
            "album" => {
                let album_query = cmd.str_opt("album").unwrap();
                let provider = cmd.str_opt("provider");
                match self.lookup_album(&album_query, provider.as_deref()).await? {
                    None => bail!("Not found"),
                    Some(mut info) => {
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
                                info.genres = self.lastfm.artist_top_tags(artist).await?;
                            }
                        }
                        if let Some(genres) = info.format_genres() {
                            _ = writeln!(&mut contents, "{}", &genres);
                        }
                        contents.push_str(info.url.as_deref().unwrap_or("no link found"));
                        Ok(Some(contents))
                    }
                }
            }
            "relative" => {
                let time = cmd.str_opt("time").expect("missing time");
                let parsed = reltime::parse_time(&time);
                let contents = format!("{} is in <t:{}:R>", time, parsed.timestamp());
                Ok(Some(contents))
            }
            "setrole" => {
                let role = cmd.role_opt("role");
                self.set_role(
                    cmd.command.guild_id.map(|g| g.0),
                    role.as_ref().map(|r| r.id.0),
                )?;
                let contents = match role {
                    Some(r) => format!("LP role changed to <@&{}>", r.id.0),
                    None => "LP role removed".to_string(),
                };
                Ok(Some(contents))
            }
            "setcreatethreads" => {
                let b = cmd.bool_opt("create_threads");
                self.set_should_create_threads(
                    cmd.command.guild_id.map(|g| g.0),
                    b.unwrap_or(false),
                )?;
                let contents = format!(
                    "LPBot will {}create threads for listening parties",
                    if b == Some(true) { "" } else { "not " }
                );
                Ok(Some(contents))
            }
            "setwebhook" => {
                let wh = cmd.str_opt("webhook");
                self.set_webhook(Some(guild_id.0), wh.as_deref())?;
                let contents = format!(
                    "LPBot will {}use a webhook",
                    if wh.is_some() { "" } else { "not " }
                );
                Ok(Some(contents))
            }
            "quote" => {
                let guild_id = cmd
                    .command
                    .guild_id
                    .ok_or_else(|| anyhow!("Must be run in a server"))?
                    .0;
                if let Some((_, message)) = cmd.command.data.resolved.messages.iter().next() {
                    let quote_number = self.add_quote(guild_id, message)?;
                    let link = message.id.link(message.channel_id, Some(GuildId(guild_id)));
                    Ok(Some(format!("Quote saved as #{}: {}", quote_number, link)))
                } else {
                    let quote = if let Some(quote_number) = cmd.int_opt("number") {
                        self.fetch_quote(guild_id, quote_number as u64)?
                    } else {
                        self.get_random_quote(guild_id)?
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
                    cmd.command
                        .create_interaction_response(&cmd.ctx.http, |resp| {
                            resp.kind(InteractionResponseType::ChannelMessageWithSource)
                                .interaction_response_data(|data| {
                                    data.embed(|embed| {
                                        embed
                                            .author(|a| a.name(format!("#{}", quote.quote_number)))
                                            .description(&contents)
                                            .url(message_url)
                                            .timestamp(quote.ts.format("%+").to_string())
                                    })
                                })
                        })
                        .await?;
                    Ok(None)
                }
            }
            _ => bail!("Unknown command"),
        }
    }

    async fn process_autocomplete(
        &self,
        ctx: &Context,
        ac: AutocompleteInteraction,
    ) -> anyhow::Result<()> {
        let mut choices: Vec<(String, String)> = vec![];
        let options = &ac.data.options;
        match ac.data.name.as_str() {
            "lp" => {
                choices = self.autocomplete_lp(options).await?;
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
}

#[async_trait]
impl EventHandler for Handler {
    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Autocomplete(ac) = interaction {
            self.process_autocomplete(&ctx, ac).await.unwrap();
        } else if let Interaction::ApplicationCommand(command) = interaction {
            let cmd = SlashCommand {
                handler: self,
                ctx: &ctx,
                command: &command,
            };
            let (contents, flags) = match self.process_command(cmd).await {
                Ok(None) => return,
                Ok(Some(s)) => (s, InteractionApplicationCommandCallbackDataFlags::empty()),
                Err(e) => {
                    eprintln!("Error processing command {}: {:?}", &command.data.name, e);
                    (
                        e.to_string(),
                        InteractionApplicationCommandCallbackDataFlags::EPHEMERAL,
                    )
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

    async fn message(&self, ctx: Context, new_message: Message) {
        let channel = match new_message.channel(&ctx.http).await {
            Ok(chan) => chan,
            Err(e) => {
                eprintln!("{}", e);
                return;
            }
        };
        let is_thread = channel
            .guild()
            .map(|g| g.kind == ChannelType::PublicThread)
            .unwrap_or(false);
        if new_message.content.starts_with(".qp") && is_thread {
            if let Err(e) = new_message
                .react(&ctx.http, ReactionType::Unicode("✅".to_string()))
                .and_then(|_| new_message.react(&ctx.http, ReactionType::Unicode("❎".to_string())))
                .await
            {
                eprintln!("Error adding reactions: {}", e);
            }
            return;
        }
        if !new_message.mentions_me(&ctx.http).await.unwrap() || new_message.author.bot {
            return;
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
            let number = self.add_quote(id.0, &message).unwrap();
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
                .create_application_command(|command| {
                    command
                        .name("relative")
                        .description("Give relative timestamp")
                        .create_option(|option| {
                            option
                                .name("time")
                                .description("Time of day e.g. 7:30pm EST")
                                .kind(ApplicationCommandOptionType::String)
                                .required(true)
                        })
                })
                .create_application_command(|command| {
                    command.name("ping").description("A ping command")
                })
        })
        .await
        .unwrap();

        ApplicationCommand::create_global_application_command(&ctx.http, |command| {
            command
                .name("lp")
                .description("Host a listening party")
                .create_option(|option| {
                    option
                        .name("album")
                        .description("What you will be listening to (e.g. band - album)")
                        .kind(ApplicationCommandOptionType::String)
                        .required(true)
                        .set_autocomplete(true)
                })
                .create_option(|option| {
                    option
                        .name("time")
                        .description("Time at which the LP will take place")
                        .kind(ApplicationCommandOptionType::String)
                })
                .create_option(|option| {
                    option
                        .name("link")
                        .description("Link to the album/playlist (Spotify, Youtube, Bandcamp...)")
                        .kind(ApplicationCommandOptionType::String)
                        .set_autocomplete(true)
                })
                .create_option(|option| {
                    let opt = option
                        .name("provider")
                        .description("Where to look for album info (defaults to spotify)")
                        .kind(ApplicationCommandOptionType::String);
                    providers.iter().for_each(|p| {
                        opt.add_string_choice(p, p);
                    });
                    opt
                })
        })
        .await
        .unwrap();
        ApplicationCommand::create_global_application_command(&ctx.http, |command| {
            command
                .name("album")
                .description("Lookup an album")
                .create_option(|option| {
                    option
                        .name("album")
                        .description("The album you are looking for (e.g. band - album)")
                        .kind(ApplicationCommandOptionType::String)
                        .required(true)
                })
                .create_option(|option| {
                    let opt = option
                        .name("provider")
                        .description("Where to look for album info (defaults to spotify)")
                        .kind(ApplicationCommandOptionType::String);
                    providers.iter().for_each(|p| {
                        opt.add_string_choice(p, p);
                    });
                    opt
                })
        })
        .await
        .unwrap();
        ApplicationCommand::create_global_application_command(&ctx.http, |command| {
            command
                .name("setrole")
                .description("Set what role to ping for listening parties")
                .create_option(|option| {
                    option
                        .name("role")
                        .description("Role to ping (leave unset to clear)")
                        .kind(ApplicationCommandOptionType::Role)
                })
                .default_member_permissions(Permissions::MANAGE_ROLES)
        })
        .await
        .unwrap();
        ApplicationCommand::create_global_application_command(&ctx.http, |command| {
            command
                .name("setcreatethreads")
                .description("Configure whether LPBot should create threads for LPs")
                .create_option(|option| {
                    option
                        .name("create_threads")
                        .description("Create threads for LPs")
                        .kind(ApplicationCommandOptionType::Boolean)
                        .required(true)
                })
                .default_member_permissions(Permissions::MANAGE_THREADS)
        })
        .await
        .unwrap();
        ApplicationCommand::create_global_application_command(&ctx.http, |command| {
            command.name("quote").kind(ApplicationCommandType::Message)
        })
        .await
        .unwrap();
        ApplicationCommand::create_global_application_command(&ctx.http, |command| {
            command
                .name("quote")
                .description("Retrieve a quote")
                .create_option(|option| {
                    option
                        .name("number")
                        .description("Number the quote was saved as (optional)")
                        .kind(ApplicationCommandOptionType::Integer)
                        .min_int_value(1)
                })
        })
        .await
        .unwrap();
        ApplicationCommand::create_global_application_command(&ctx.http, |command| {
            command
                .name("setwebhook")
                .description(
                    "Set (or unset) a webhook for LPBot to use when creating listening parties",
                )
                .create_option(|option| {
                    option
                        .name("webhook")
                        .description("The webhook URL (leave empty to remove)")
                        .kind(ApplicationCommandOptionType::String)
                })
                .default_member_permissions(Permissions::MANAGE_WEBHOOKS)
        })
        .await
        .unwrap();
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

    // Configure the client with your Discord bot token in the environment.
    let token = env::var("DISCORD_TOKEN").expect("Expected a token in the environment");

    // The Application Id is usually the Bot User Id.
    let application_id: u64 = env::var("APPLICATION_ID")
        .expect("Expected an application id in the environment")
        .parse()
        .expect("application id is not a valid id");

    // Build our client.
    let mut client = Client::builder(
        token,
        GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::GUILD_MESSAGE_REACTIONS
            | GatewayIntents::MESSAGE_CONTENT,
    )
    .event_handler(handler)
    .application_id(application_id)
    .await
    .expect("Error creating client");

    // Finally, start a single shard, and start listening to events.
    //
    // Shards will automatically attempt to reconnect, and will perform
    // exponential backoff until it reconnects.
    if let Err(why) = client.start().await {
        println!("Client error: {:?}", why);
    }
}
