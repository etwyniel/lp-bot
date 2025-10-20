use std::env;
use std::ops::DerefMut;
use std::sync::Arc;

use anyhow::{Context as _, anyhow};
use rspotify::scopes;
use rusqlite::Connection;
use serenity::all::{ApplicationId, CreateAttachment, InteractionResponseFlags};
use serenity::builder::{CreateAllowedMentions, CreateMessage};
use serenity::model::channel::Channel;
use serenity::model::event::ChannelPinsUpdateEvent;
use serenity::model::prelude::{Message, UserId};
use serenity::{
    async_trait,
    model::{
        application::Command, application::Interaction, gateway::GatewayIntents, gateway::Ready,
        id::GuildId,
    },
    prelude::*,
};

use serenity_command::ContentAndFlags;
use serenity_command_handler::modules::bdays::Bdays;
use serenity_command_handler::modules::quotes::{self, GetQuote};
use serenity_command_handler::modules::sql::{Query, Sql};
use serenity_command_handler::modules::{
    AlbumLookup, Forms, ModAutoreacts, ModLp, ModPoll, Pinboard, PlaylistBuilder, Quotes,
    SpotifyOAuth, autoreact, bdays, forms, polls, spotify,
};

use serenity_command_handler::Handler;

struct HandlerWrapper(Handler);

impl HandlerWrapper {
    #[allow(unused)]
    async fn delete_guild_commands(
        &self,
        ctx: &Context,
        guilds: impl Iterator<Item = GuildId>,
        command_names: &[&str],
    ) -> anyhow::Result<()> {
        for g in guilds {
            for cmd in g.get_commands(&ctx.http).await? {
                if command_names.contains(&cmd.name.as_str()) {
                    g.delete_command(&ctx.http, cmd.id).await?;
                }
            }
        }
        Ok(())
    }

    async fn process_message(&self, ctx: Context, msg: Message) -> anyhow::Result<()> {
        // spotify::handle_message(&ctx.http, &msg).await?;
        let lower = msg.content.to_lowercase();
        if lower.starts_with(".fmcrabdown") || lower.starts_with(".crabdown") {
            let module: Arc<ModPoll> = self.0.module_arc()?;
            polls::crabdown(
                module,
                &ctx.http,
                msg.channel_id,
                None,
                None,
                &self.0.event_handlers,
            )
            .await?;
            return Ok(());
        }
        if let Some(mut params) = msg.content.strip_prefix(".lpquote") {
            let mut hide_author = None;
            if let Some(suffix) = params.strip_prefix("_trivia") {
                params = suffix;
                hide_author = Some(true);
            }
            let params = params.trim();
            let mut user: Option<UserId> = params.parse().ok();
            let mut number: Option<i64> = params.parse().ok();
            if number.map(|n| n > 1000000).unwrap_or(false) {
                // Treating n as a user ID
                user = number.take().map(|n| UserId::new(n as u64));
            }
            let resp = GetQuote {
                number,
                user,
                hide_author,
            }
            .get_quote(
                &self.0,
                &ctx,
                msg.guild_id
                    .ok_or_else(|| anyhow!("Must be run in a server"))?
                    .get(),
            )
            .await?;

            let ContentAndFlags(contents, embeds, attachments, _) =
                match resp.to_contents_and_flags() {
                    None => return Ok(()),
                    Some(c) => c,
                };
            let mut create_msg = CreateMessage::new()
                .content(contents)
                .embeds(embeds.into_iter().flatten().collect())
                .allowed_mentions(CreateAllowedMentions::new().empty_roles().empty_users());
            for att in attachments.iter().flatten() {
                create_msg = create_msg.add_file(CreateAttachment::url(&ctx.http, att).await?);
            }
            msg.channel_id.send_message(&ctx.http, create_msg).await?;
        } else if let Some(query) = msg.content.strip_prefix(".qry") {
            let db = self.0.db.lock().await;
            let Some(ContentAndFlags(contents, embeds, _, _)) = (match (Query {
                qry: query.trim().to_string(),
            }
            .query(db.conn(), msg.author.id, false))
            {
                Ok(resp) => resp.to_contents_and_flags(),
                Err(e) => Some(ContentAndFlags(
                    e.to_string(),
                    None,
                    None,
                    InteractionResponseFlags::empty(),
                )),
            }) else {
                return Ok(());
            };
            let msg_id = &msg;
            msg.channel_id
                .send_message(
                    &ctx.http,
                    CreateMessage::new()
                        .reference_message(msg_id)
                        .content(contents)
                        .add_embeds(embeds.into_iter().flatten().collect())
                        .allowed_mentions(CreateAllowedMentions::new().empty_roles().empty_users()),
                )
                .await?;
        }
        autoreact::add_reacts(&self.0, &ctx, msg).await
    }
}

#[async_trait]
impl EventHandler for HandlerWrapper {
    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        self.0.process_interaction(ctx, interaction).await;
    }

    async fn reaction_add(&self, ctx: Context, add_reaction: serenity::model::channel::Reaction) {
        ModPoll::handle_ready_poll(&self.0, &ctx, &add_reaction)
            .await
            .unwrap();
        _ = spotify::handle_reaction(&self.0, &ctx.http, &add_reaction).await;
        if !add_reaction.emoji.unicode_eq("🗨️") {
            return;
        }
        if let Some(id) = add_reaction.guild_id {
            let message = match add_reaction.message(&ctx.http).await {
                Ok(m) => m,
                Err(_) => return,
            };
            let number = match quotes::add_quote(&self.0, &ctx, id.get(), &message).await {
                Ok(Some(n)) => n,
                Ok(None) => return,
                Err(e) => {
                    eprintln!("Error adding quote: {e:?}");
                    return;
                }
            };
            if let Ok(Channel::Guild(g)) = add_reaction.channel(&ctx.http).await {
                g.send_message(
                    &ctx.http,
                    CreateMessage::new()
                        .reference_message((g.id, message.id))
                        .allowed_mentions(CreateAllowedMentions::new().empty_users())
                        .content(format!("Quote saved as #{number}")),
                )
                .await
                .unwrap();
            }
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        let commands = Command::get_global_commands(&ctx.http).await.unwrap();
        for cmd in commands {
            if cmd.name == "build_playlist" {
                Command::delete_global_command(&ctx.http, cmd.id)
                    .await
                    .unwrap();
            }
        }
        tokio::spawn(bdays::bday_loop(
            Arc::clone(&self.0.db),
            Arc::clone(&ctx.http),
        ));
        tokio::spawn(quotes::qotd_loop(
            Arc::clone(&self.0.db),
            Arc::clone(&ctx.http),
        ));
        self.0.self_id.set(ready.user.id).unwrap();
        eprintln!("{} is running!", &ready.user.name);
        for runner in self.0.commands.read().await.0.values() {
            if let Some(guild) = runner.guild() {
                if let Err(e) = guild.create_command(&ctx.http, runner.register()).await {
                    eprintln!("error creating command {}: {}", runner.name().0, e);
                    panic!()
                }
            } else if let Err(e) =
                Command::create_global_command(&ctx.http, runner.register()).await
            {
                eprintln!("error creating command {}: {}", runner.name().0, e);
                panic!()
            }
        }
        forms::check_forms(&self.0, &ctx).await.unwrap();
    }

    async fn message(&self, ctx: Context, new_message: Message) {
        if let Err(e) = self.process_message(ctx, new_message).await {
            eprintln!("Error processing message: {e:?}");
        }
    }

    async fn channel_pins_update(&self, ctx: Context, pin: ChannelPinsUpdateEvent) {
        let guild_id = match pin.guild_id {
            Some(gid) => gid,
            None => return,
        };
        if let Err(e) =
            Pinboard::move_pin_to_pinboard(&self.0, &ctx, pin.channel_id, guild_id).await
        {
            eprintln!("Error moving message to pinboard: {e:?}");
        }
    }
}

#[tokio::main]
async fn main() {
    let conn = Connection::open("lpbot.sqlite").unwrap();
    let spotify_oauth = SpotifyOAuth::new_auth_code(scopes!(
        "playlist-modify-public",
        "playlist-read-private",
        "playlist-read-collaborative",
        "user-library-read",
        "user-read-private",
        "playlist-modify-private"
    ))
    .await
    .context("spotify client")
    .unwrap();
    let management_guild = env::var("MANAGEMENT_GUILD_ID")
        .context("env variable MANAGEMENT_GUILD_ID missing")
        .unwrap()
        .parse::<u64>()
        .map(GuildId::new)
        .context("Failed to parse MANAGEMENT_GUILD_ID")
        .unwrap();

    let handler = Handler::builder(conn, management_guild)
        .await
        .with_module(spotify_oauth)
        .await
        .unwrap()
        .module::<ModLp>()
        .await
        .unwrap()
        .module::<Quotes>()
        .await
        .unwrap()
        .module::<ModAutoreacts>()
        .await
        .unwrap()
        .module::<Pinboard>()
        .await
        .unwrap()
        .module::<AlbumLookup>()
        .await
        .unwrap()
        .module::<Bdays>()
        .await
        .unwrap()
        .module::<ModPoll>()
        .await
        .unwrap()
        .module::<Sql>()
        .await
        .unwrap()
        .module::<Forms>()
        .await
        .unwrap()
        .module::<PlaylistBuilder>()
        .await
        .unwrap()
        .default_command_handler(Forms::process_form_command)
        .build();

    handler
        .module::<ModAutoreacts>()
        .unwrap()
        .load_reacts(handler.db.lock().await.deref_mut())
        .await
        .unwrap();

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
    .event_handler(HandlerWrapper(handler))
    .application_id(ApplicationId::new(application_id))
    .await
    .expect("Error creating client");

    // Start a single shard, and start listening to events.
    //
    // Shards will automatically attempt to reconnect, and will perform
    // exponential backoff until it reconnects.
    if let Err(why) = client.start().await {
        println!("Client error: {why:?}");
    }
}
