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

    fn set_role(&self, guild_id: Option<u64>, role_id: Option<u64>) -> anyhow::Result<()> {
        let guild_id = match guild_id {
            Some(id) => id,
            None => return Err(anyhow!("Must be run in a server")),
        };
        let db = self.db.lock().unwrap();
        match role_id {
            None => db.execute("DELETE  FROM guild WHERE id = ?1", params![guild_id]),
            Some(id) => db.execute(
                "INSERT INTO guild (id, role_id) VALUES (?1, ?2)
                ON CONFLICT(id) DO UPDATE SET role_id = ?2",
                params![guild_id, id],
            ),
        }
        .map(|_| ())
        .map_err(anyhow::Error::from)
    }
}

impl<'a, 'b> Command<'a, 'b> {
    fn str_opt(&self, name: &str) -> Option<String> {
        for opt in &self.command.data.options {
            if opt.name == name {
                if let Some(ApplicationCommandInteractionDataOptionValue::String(s)) = &opt.resolved
                {
                    return Some(s.to_string());
                }
                return None;
            }
        }
        None
    }

    fn role_opt(&self, name: &str) -> Option<Role> {
        match self
            .command
            .data
            .options
            .iter()
            .find(|opt| opt.name == name)
            .and_then(|opt| opt.resolved.as_ref())
        {
            Some(ApplicationCommandInteractionDataOptionValue::Role(r)) => Some(r.clone()),
            _ => None,
        }
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
    }
}

fn init_db() -> anyhow::Result<Connection> {
    let conn = Connection::open("lpbot.sqlite")?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS guild (
            id INTEGER PRIMARY KEY,
            role_id INTEGER
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
