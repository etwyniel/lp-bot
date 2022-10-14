use std::collections::HashMap;
use std::env;
use std::fmt::Write;
use std::ops::Deref;
use std::sync::Arc;
use std::time::{Duration, Instant};

use album::Album;
use anyhow::{anyhow, bail};
use autoreact::ReactsCache;
use chrono::{Datelike, Local, Timelike, Utc};
use commands::{ready_poll, GetQuote};
use fallible_iterator::FallibleIterator;
use lastfm::Lastfm;
use rusqlite::Connection;
use serenity::builder::CreateApplicationCommandOption;
use serenity::http::Http;
use serenity::model::application::command::CommandType;
use serenity::model::application::interaction::autocomplete::AutocompleteInteraction;
use serenity::model::channel::Channel;
use serenity::model::event::ChannelPinsUpdateEvent;
use serenity::model::id::ChannelId;
use serenity::model::prelude::interaction::application_command::{
    ApplicationCommandInteraction, CommandDataOption, CommandDataOptionValue,
};
use serenity::model::prelude::{Message, UserId};
use serenity::{
    async_trait,
    model::{
        application::command::{Command, CommandOptionType},
        application::interaction::Interaction,
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
mod pinboard;
mod reltime;
mod spotify;

use album::AlbumProvider;
use bandcamp::Bandcamp;
use command_context::{get_focused_option, get_str_opt_ac, Responder, SlashCommand, TextCommand};
use db::Birthday;
use serenity_command::{BotCommand, CommandBuilder, CommandResponse, CommandRunner};
use spotify::Spotify;
use tokio::sync::{self, OnceCell};
use tokio::time::interval;

pub struct Handler {
    db: Arc<sync::Mutex<Connection>>,
    spotify: Arc<Spotify>,
    providers: Vec<Box<dyn AlbumProvider>>,
    lastfm: Arc<Lastfm>,
    reacts_cache: RwLock<ReactsCache>,
    commands: RwLock<HashMap<&'static str, Box<dyn CommandRunner<Handler> + Send + Sync>>>,
    http: OnceCell<Arc<Http>>,
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
        let conn = Arc::new(sync::Mutex::new(db::init()?));
        let spotify = Arc::new(Spotify::new().await?);
        let providers = vec![
            Box::new(Arc::clone(&spotify)) as Box<dyn AlbumProvider>,
            Box::new(Bandcamp::new()),
        ];
        let lastfm = Arc::new(Lastfm::new());
        let reacts_cache = RwLock::new(autoreact::new(conn.lock().await.deref()).await?);
        let commands = RwLock::new(Self::init_commands());
        let handler = Handler {
            db: conn,
            spotify,
            providers,
            lastfm,
            reacts_cache,
            commands,
            http: OnceCell::new(),
        };
        Ok(handler)
    }

    fn http(&self) -> &Http {
        self.http.get().unwrap().as_ref()
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

    async fn process_command(
        &self,
        ctx: &Context,
        cmd: SlashCommand<'_, '_>,
    ) -> anyhow::Result<CommandResponse> {
        let guild_id = cmd
            .command
            .guild_id
            .ok_or_else(|| anyhow!("Must be run in a server"))?
            .0;
        let data = &cmd.command.data;
        let http = self.http();
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
                        .run(self, ctx, cmd.command)
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
                    .create_interaction_response(http, |resp| {
                        resp.interaction_response_data(|data| {
                            let header = if let Some(server) =
                                cmd.command.guild_id.and_then(|g| g.name(ctx))
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
                    .create_interaction_response(http, |resp| {
                        resp.interaction_response_data(|data| data.content("Processing image..."))
                    })
                    .await?;
                let magiked = magik::magik(&url, scale)?;
                cmd.command
                    .create_followup_message(http, |msg| {
                        msg.add_file((magiked.as_slice(), "out.png"))
                    })
                    .await?;
                Ok(CommandResponse::None)
            }
            _ => {
                if let Some(runner) = self.commands.read().await.get(cmd.name()) {
                    runner.run(self, ctx, cmd.command).await
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

    async fn crabdown(&self, http: &Http, channel: ChannelId) -> anyhow::Result<()> {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.tick().await;
        for i in 0..3 {
            channel
                .send_message(http, |msg| msg.content("🦀".repeat(3 - i)))
                .await?;
            interval.tick().await;
        }
        channel
            .send_message(http, |msg| msg.content("<a:CrabRave:988508208240922635>"))
            .await?;
        Ok(())
    }

    #[allow(unused)]
    async fn delete_guild_commands(
        &self,
        ctx: &Context,
        guilds: impl Iterator<Item = GuildId>,
        command_names: &[&str],
    ) -> anyhow::Result<()> {
        for g in guilds {
            for cmd in g.get_application_commands(&ctx.http).await? {
                if command_names.contains(&cmd.name.as_str()) {
                    g.delete_application_command(&ctx.http, cmd.id).await?;
                }
            }
        }
        Ok(())
    }

    async fn process_message(&self, ctx: Context, msg: Message) -> anyhow::Result<()> {
        let lower = msg.content.to_lowercase();
        if &lower == ".fmcrabdown" || &lower == ".crabdown" {
            self.crabdown(&ctx.http, msg.channel_id).await?;
            return Ok(());
        } else if let Some(params) = msg.content.strip_prefix(".lpquote") {
            let params = params.trim();
            let mut user: Option<UserId> = params.parse().ok();
            let mut number: Option<i64> = params.parse().ok();
            if number.map(|n| n > 1000000).unwrap_or(false) {
                user = number.take().map(|n| UserId(n as u64));
            }
            let resp = GetQuote { number, user }
                .get_quote(
                    self,
                    &ctx,
                    msg.guild_id
                        .ok_or_else(|| anyhow!("Must be run in a server"))?
                        .0,
                )
                .await?;
            TextCommand {
                handler: self,
                message: &msg,
            }
            .respond(&ctx.http, resp, None)
            .await?;
        }
        self.add_reacts(&ctx, msg).await
    }
}

async fn wish_bday(http: &Http, user_id: u64, guild_id: GuildId) -> anyhow::Result<()> {
    let member = guild_id.member(http, user_id).await?;
    let channels = guild_id.channels(http).await?;
    let channel = channels
        .values()
        .find(|chan| chan.name() == "general")
        .or_else(|| {
            channels
                .values()
                .find(|chan| chan.position == 0 || chan.position == -1)
        })
        .ok_or_else(|| anyhow!("Could not find a suitable channel"))?;
    channel
        .say(http, format!("Happy birthday to <@{}>!", member.user.id.0))
        .await?;
    Ok(())
}

async fn bday_loop(db: Arc<Mutex<Connection>>, http: Arc<Http>) {
    let mut interval = interval(Duration::from_secs(3600));
    loop {
        interval.tick().await;
        let now = Local::now();
        if now.hour() != 10 {
            continue;
        }
        let guilds_and_users = {
            let db = db.lock().await;
            let mut stmt = db
                .prepare("SELECT guild_id, user_id FROM bdays WHERE day = ?1 AND month = ?2")
                .unwrap();
            stmt.query([now.day(), now.month()])
                .unwrap()
                .map(|row| Ok((row.get(0)?, row.get(1)?)))
                .iterator()
                .filter_map(Result::ok)
                .collect::<Vec<_>>()
        };
        for (guild_id, user_id) in guilds_and_users {
            if let Err(e) = wish_bday(http.as_ref(), user_id, GuildId(guild_id)).await {
                eprintln!("Error wishing user birthday: {:?}", e);
            }
        }
    }
}

// Format command options for debug output
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
                command: &command,
            };
            let start = Instant::now();
            let resp = self.process_command(&ctx, cmd).await;
            let elapsed = start.elapsed();
            eprintln!("{guild_name}{user}: /{name} -({:?})-> {:?}", elapsed, &resp);
            let resp = match resp {
                Ok(resp) => resp,
                Err(e) => CommandResponse::Private(e.to_string()),
            };

            if let Err(why) = command.respond(&ctx.http, resp, None).await {
                eprintln!("cannot respond to slash command: {:?}", why);
                return;
            }
        }
    }

    async fn reaction_add(&self, ctx: Context, add_reaction: serenity::model::channel::Reaction) {
        ready_poll::handle_ready_poll(self, &add_reaction)
            .await
            .unwrap();
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
                    eprintln!("Error adding quote: {:?}", e);
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
        println!(
            "{} is connected to {} guilds!",
            ready.user.name,
            ready.guilds.len()
        );
        self.http.set(Arc::clone(&ctx.http)).unwrap();
        _ = tokio::spawn(bday_loop(Arc::clone(&self.db), Arc::clone(&ctx.http)));

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
            command.name("quote").kind(CommandType::Message)
        })
        .await
        .unwrap();
        Command::create_global_application_command(&ctx.http, |command| {
            commands::GetQuote::runner().register(command)
        })
        .await
        .unwrap();

        for runner in self.commands.read().await.values() {
            Command::create_global_application_command(&ctx.http, |command| {
                runner.register(command)
            })
            .await
            .unwrap();
        }
    }

    async fn message(&self, ctx: Context, new_message: Message) {
        if let Err(e) = self.process_message(ctx, new_message).await {
            eprintln!("Error processing message: {:?}", e);
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
            eprintln!("Error moving message to pinboard: {:?}", e);
        }
    }
}

#[tokio::main]
async fn main() {
    let handler = match Handler::new().await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Initialization failed: {:?}", e);
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
