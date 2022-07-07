use std::{env, sync};

use anyhow::anyhow;
use lp::Command;
use rspotify::{
    model::{AlbumId, SearchType, SimplifiedAlbum},
    prelude::*,
    ClientCredsSpotify, Credentials,
};
use rusqlite::{params, Connection};
use serenity::{
    async_trait,
    model::{
        channel::Message,
        gateway::GatewayIntents,
        gateway::Ready,
        guild::Role,
        id::GuildId,
        interactions::{
            application_command::{
                ApplicationCommand, ApplicationCommandInteractionDataOptionValue,
                ApplicationCommandOptionType,
            },
            Interaction, InteractionApplicationCommandCallbackDataFlags, InteractionResponseType,
        },
        permissions::Permissions,
    },
    prelude::*,
};
mod lp;
mod reltime;

pub struct Handler {
    db: sync::Mutex<Connection>,
    spotify: rspotify::ClientCredsSpotify,
}

fn album_info(album: &SimplifiedAlbum) -> Option<(String, String)> {
    let url = album.id.as_ref()?.url();
    let title = match &album
        .album_group
        .as_ref()
        .or_else(|| album.artists.first().map(|a| &a.name))
    {
        Some(grp) => format!("{} - {}", grp, &album.name),
        None => album.name.clone(),
    };
    Some((url, title))
}

impl Handler {
    async fn lookup_album(&self, query: &str) -> anyhow::Result<Option<(String, String)>> {
        self.spotify.auto_reauth().await?;
        let res = self
            .spotify
            .search(query, &SearchType::Album, None, None, Some(1), None)
            .await?;
        if let rspotify::model::SearchResult::Albums(albums) = res {
            Ok(albums.items.first().and_then(album_info))
        } else {
            Err(anyhow!("Album not found"))
        }
    }

    async fn get_album_info(&self, link: &str) -> anyhow::Result<Option<String>> {
        if let Some(id) = link.strip_prefix("https://open.spotify.com/album/") {
            self.spotify.auto_reauth().await?;
            let album_id = id.split('?').next().unwrap();
            let album = self.spotify.album(&AlbumId::from_id(album_id)?).await?;
            let title = match &album.artists.first().map(|a| &a.name) {
                Some(grp) => format!("{} - {}", grp, &album.name),
                None => album.name.clone(),
            };
            return Ok(Some(title));
        }
        Ok(None)
    }

    async fn new() -> anyhow::Result<Self> {
        let conn = init_db()?;

        let creds = Credentials::from_env().ok_or(anyhow!("No spotify credentials"))?;
        let mut spotify = ClientCredsSpotify::new(creds);

        // Obtaining the access token
        spotify.request_token().await?;
        spotify.auto_reauth().await?;
        Ok(Handler {
            db: sync::Mutex::new(conn),
            spotify,
        })
    }

    fn ensure_guild_table(&self, guild_id: u64) -> anyhow::Result<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT INTO guild (id) VALUES (?1) ON CONFLICT(id) DO NOTHING",
            params![guild_id],
        )?;
        Ok(())
    }

    fn set_role(&self, guild_id: Option<u64>, role_id: Option<u64>) -> anyhow::Result<()> {
        let guild_id = match guild_id {
            Some(id) => id,
            None => return Err(anyhow!("Must be run in a server")),
        };
        self.ensure_guild_table(guild_id)?;
        let db = self.db.lock().unwrap();
        db.execute(
            "UPDATE guild SET role_id = ?1 WHERE id = ?2",
            params![role_id, guild_id],
        )?;
        Ok(())
    }

    fn set_should_create_threads(&self, guild_id: Option<u64>, create: bool) -> anyhow::Result<()> {
        let guild_id = match guild_id {
            Some(id) => id,
            None => return Err(anyhow!("Must be run in a server")),
        };
        self.ensure_guild_table(guild_id)?;
        let db = self.db.lock().unwrap();
        db.execute(
            "UPDATE guild SET create_threads = ?1 WHERE id = ?2",
            params![create, guild_id],
        )?;
        Ok(())
    }
}

impl<'a, 'b> Command<'a, 'b> {
    fn opt<T>(
        &self,
        name: &str,
        getter: impl FnOnce(&ApplicationCommandInteractionDataOptionValue) -> Option<T>,
    ) -> Option<T> {
        match self
            .command
            .data
            .options
            .iter()
            .find(|opt| opt.name == name)
            .and_then(|opt| opt.resolved.as_ref())
        {
            Some(o) => getter(o),
            _ => None,
        }
    }

    fn str_opt(&self, name: &str) -> Option<String> {
        self.opt(name, |o| {
            if let ApplicationCommandInteractionDataOptionValue::String(s) = o {
                Some(s.clone())
            } else {
                None
            }
        })
    }

    fn role_opt(&self, name: &str) -> Option<Role> {
        self.opt(name, |o| {
            if let ApplicationCommandInteractionDataOptionValue::Role(r) = o {
                Some(r.clone())
            } else {
                None
            }
        })
    }

    fn bool_opt(&self, name: &str) -> Option<bool> {
        self.opt(name, |o| {
            if let ApplicationCommandInteractionDataOptionValue::Boolean(b) = o {
                Some(*b)
            } else {
                None
            }
        })
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::ApplicationCommand(command) = interaction {
            let cmd = Command {
                handler: self,
                ctx: &ctx,
                command: &command,
            };
            let content = match command.data.name.as_str() {
                "lp" => {
                    let lp_name = cmd.str_opt("album");
                    let time = cmd.str_opt("time");
                    let link = cmd.str_opt("link");
                    match cmd.lp(lp_name, link, time).await {
                        Err(e) => {
                            dbg!(&e);
                            e.to_string()
                        }
                        _ => return,
                    }
                }
                "relative" => {
                    let time = cmd.str_opt("time").expect("missing time");
                    let parsed = reltime::parse_time(&time);
                    format!("{} is in <t:{}:R>", time, parsed.timestamp())
                }
                "setrole" => {
                    let role = cmd.role_opt("role");
                    match self
                        .set_role(command.guild_id.map(|g| g.0), role.as_ref().map(|r| r.id.0))
                    {
                        Err(e) => {
                            dbg!(&e);
                            e.to_string()
                        }
                        _ => match role {
                            Some(r) => format!("LP role changed to <@&{}>", r.id.0),
                            None => format!("LP role removed"),
                        },
                    }
                }
                "setcreatethreads" => {
                    let b = cmd.bool_opt("create_threads");
                    match self.set_should_create_threads(
                        command.guild_id.map(|g| g.0),
                        b.unwrap_or(false),
                    ) {
                        Err(e) => {
                            dbg!(&e);
                            e.to_string()
                        }
                        _ => format!(
                            "LPBot will {}create threads for listening parties",
                            if b == Some(true) { "" } else { "not " }
                        ),
                    }
                }
                _ => "not implemented :(".to_string(),
            };

            if let Err(why) = command
                .create_interaction_response(&ctx.http, |response| {
                    response
                        .kind(InteractionResponseType::ChannelMessageWithSource)
                        .interaction_response_data(|message| {
                            message
                                .content(content)
                                .flags(InteractionApplicationCommandCallbackDataFlags::EPHEMERAL)
                        })
                })
                .await
            {
                println!("cannot respond to slash command: {}", why);
                return;
            }
        }
    }

    async fn message(&self, ctx: Context, new_message: Message) {
        if !new_message.mentions_me(&ctx.http).await.unwrap() || new_message.author.bot {
            return;
        }
        if let Err(e) = self.text_command_lp(&ctx, new_message).await {
            eprintln!("Failed to start LP from text command: {}", e);
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
    }
}

fn init_db() -> anyhow::Result<Connection> {
    let conn = Connection::open("lpbot.sqlite")?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS guild (
            id INTEGER PRIMARY KEY,
            role_id INTEGER,
            create_threads BOOLEAN NOT NULL DEFAULT(TRUE)
        )",
        [],
    )?;
    Ok(conn)
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
    let mut client = Client::builder(token, GatewayIntents::GUILD_MESSAGES)
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
