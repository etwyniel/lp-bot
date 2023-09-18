use std::env;
use std::sync::Arc;

use anyhow::anyhow;
use rusqlite::Connection;
use serenity::model::channel::Channel;
use serenity::model::event::ChannelPinsUpdateEvent;
use serenity::model::prelude::interaction::application_command::ApplicationCommandInteraction;
use serenity::model::prelude::{Message, UserId};
use serenity::{
    async_trait,
    model::{
        application::command::Command, application::interaction::Interaction,
        gateway::GatewayIntents, gateway::Ready, id::GuildId,
    },
    prelude::*,
};

mod magik;
mod reltime;

use serenity_command_handler::modules::quotes::{self, GetQuote};
use serenity_command_handler::modules::{
    autoreact, bdays, polls, AlbumLookup, ModAutoreacts, ModLp, ModPoll, Pinboard, Quotes,
};

use serenity_command_handler::Handler;

struct HandlerWrapper(Handler);

trait InteractionExt {
    fn guild_id(&self) -> anyhow::Result<GuildId>;
}

impl InteractionExt for ApplicationCommandInteraction {
    fn guild_id(&self) -> anyhow::Result<GuildId> {
        self.guild_id
            .ok_or_else(|| anyhow!("Must be run in a server"))
    }
}

impl HandlerWrapper {
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
        if lower.starts_with(".fmcrabdown") || lower.starts_with(".crabdown") {
            let module: Arc<ModPoll> = self.0.module_arc()?;
            polls::crabdown(module, &ctx.http, msg.channel_id, None, None).await?;
            return Ok(());
        } else if let Some(mut params) = msg.content.strip_prefix(".lpquote") {
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
                user = number.take().map(|n| UserId(n as u64));
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
                    .0,
            )
            .await?;

            let (contents, embeds, _) = match resp.to_contents_and_flags() {
                None => return Ok(()),
                Some(c) => c,
            };
            msg.channel_id
                .send_message(&ctx.http, |msg| {
                    msg.add_embeds(embeds.into_iter().collect());
                    msg.content(contents)
                        .allowed_mentions(|mentions| mentions.empty_roles().empty_users())
                })
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
        if !add_reaction.emoji.unicode_eq("🗨️") {
            return;
        }
        if let Some(id) = add_reaction.guild_id {
            let message = match add_reaction.message(&ctx.http).await {
                Ok(m) => m,
                Err(_) => return,
            };
            let number = match quotes::add_quote(&self.0, &ctx, id.0, &message).await {
                Ok(Some(n)) => n,
                Ok(None) => return,
                Err(e) => {
                    eprintln!("Error adding quote: {e:?}");
                    return;
                }
            };
            if let Ok(Channel::Guild(g)) = add_reaction.channel(&ctx.http).await {
                g.send_message(&ctx.http, |m| {
                    m.reference_message((g.id, message.id))
                        .allowed_mentions(|mentions| mentions.empty_users())
                        .content(&format!("Quote saved as #{number}"))
                })
                .await
                .unwrap();
            }
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        let commands = Command::get_global_application_commands(&ctx.http)
            .await
            .unwrap();
        for cmd in commands {
            if cmd.name == "build_playlist" {
                Command::delete_global_application_command(&ctx.http, cmd.id)
                    .await
                    .unwrap();
            }
        }
        tokio::spawn(bdays::bday_loop(
            Arc::clone(&self.0.db),
            Arc::clone(&ctx.http),
        ));
        self.0.self_id.set(ready.user.id).unwrap();
        eprintln!("{} is running!", &ready.user.name);
        for runner in self.0.commands.read().await.0.values() {
            if let Some(guild) = runner.guild() {
                if let Err(e) = guild
                    .create_application_command(&ctx.http, |command| runner.register(command))
                    .await
                {
                    eprintln!("error creating command {}: {}", runner.name().0, e);
                    panic!()
                }
            } else if let Err(e) =
                Command::create_global_application_command(&ctx.http, |command| {
                    runner.register(command)
                })
                .await
            {
                eprintln!("error creating command {}: {}", runner.name().0, e);
                panic!()
            }
        }
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
    let handler = Handler::builder(conn)
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
        .module::<ModPoll>()
        .await
        .unwrap()
        .build();

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
    .application_id(application_id)
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
