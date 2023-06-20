use std::collections::VecDeque;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context as _};
use itertools::Itertools;
use serenity::builder::CreateApplicationCommandOption;
use serenity::http::Http;
use serenity::model::id::InteractionId;
use serenity::model::prelude::interaction::application_command::ApplicationCommandInteraction;
use serenity::model::prelude::{ChannelId, Reaction, ReactionType};
use serenity::prelude::Mutex;
use serenity::{async_trait, prelude::Context};
use serenity_command::{BotCommand, CommandBuilder, CommandResponse};
use serenity_command_derive::Command;

use crate::{CommandStore, CompletionStore, Handler, Module, ModuleMap};

const YES: &str = "<:FeelsGoodCrab:988509541069127780>";
const NO: &str = "<:FeelsBadCrab:988508541499342918>";
const START: &str = "<a:CrabRave:988508208240922635>";
const COUNT: &str = "🦀";
const GO: &str = "<a:CrabRave:988508208240922635>";

const MAX_POLLS: usize = 20;

pub type PendingPolls = VecDeque<(InteractionId, Option<String>, Option<String>, Vec<String>)>;

#[derive(Command, Debug)]
#[cmd(name = "ready_poll", desc = "Poll to start a listening party")]
pub struct ReadyPoll {
    #[cmd(desc = "Count emote")]
    pub count_emote: Option<String>,
    #[cmd(desc = "Emote Go")]
    pub go_emote: Option<String>,
}

impl ReadyPoll {
    async fn create_poll(
        self,
        handler: &Handler,
        ctx: &Context,
        interaction: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let module: &ModPoll = handler.module()?;
        {
            let mut polls = module.ready_polls.lock().await;
            while polls.len() >= MAX_POLLS {
                polls.pop_back();
            }
            polls.push_front((interaction.id, self.count_emote, self.go_emote, Vec::new()));
        }
        let http = &ctx.http;
        interaction
            .create_interaction_response(http, |msg| {
                msg.interaction_response_data(|data| {
                    data.content("Ready?")
                        .allowed_mentions(|mentions| mentions.empty_users())
                })
            })
            .await
            .context("error creating response")?;
        let resp = interaction.get_interaction_response(http).await?;
        resp.react(http, ReactionType::from_str(&module.yes)?)
            .await
            .context("error adding yes react")?;
        resp.react(http, ReactionType::from_str(&module.no)?)
            .await
            .context("error adding no react")?;
        resp.react(http, ReactionType::from_str(&module.start)?)
            .await
            .context("error adding go react")?;
        Ok(CommandResponse::None)
    }
}

fn build_message(usernames: &[String]) -> String {
    let mut msg = "Ready?".to_string();
    if usernames.is_empty() {
        return msg;
    }
    msg.push_str(" (");
    msg.push_str(&usernames.join(", "));
    if usernames.len() == 1 {
        msg.push_str(" is");
    } else {
        msg.push_str(" are");
    }
    msg.push_str(" ready)");
    msg
}

#[async_trait]
impl BotCommand for ReadyPoll {
    type Data = Handler;

    async fn run(
        self,
        handler: &Handler,
        ctx: &Context,
        interaction: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let resp = match self.create_poll(handler, ctx, interaction).await {
            Ok(CommandResponse::Public(s) | CommandResponse::Private(s)) => Some(s),
            Err(e) => {
                dbg!(&e);
                Some(e.to_string())
            }
            _ => None,
        };
        if let Some(resp) = resp {
            interaction
                .edit_original_interaction_response(&ctx.http, |msg| {
                    msg.content(resp).allowed_mentions(|m| m.empty_users())
                })
                .await?;
        }
        Ok(CommandResponse::None)
    }
}

pub async fn crabdown(
    handler: &Handler,
    http: &Http,
    channel: ChannelId,
    count_emote: Option<&str>,
    go_emote: Option<&str>,
) -> anyhow::Result<()> {
    channel.say(http, "Starting 3s countdown").await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.tick().await;
    let module: &ModPoll = handler.module()?;
    let count_emote = count_emote.unwrap_or(&module.count);
    let go_emote = go_emote.unwrap_or(&module.go);
    for i in 0..3 {
        let contents = std::iter::repeat(count_emote).take(3 - i).join(" ");
        channel
            .send_message(http, |msg| msg.content(contents))
            .await?;
        interval.tick().await;
    }
    channel
        .send_message(http, |msg| msg.content(go_emote))
        .await?;
    Ok(())
}

pub struct ModPoll {
    pub yes: String,
    pub no: String,
    pub start: String,
    pub count: String,
    pub go: String,
    ready_polls: Arc<Mutex<PendingPolls>>,
}

impl ModPoll {
    pub fn new<
        'a,
        S1: Into<Option<&'a str>>,
        S2: Into<Option<&'a str>>,
        S3: Into<Option<&'a str>>,
        S4: Into<Option<&'a str>>,
        S5: Into<Option<&'a str>>,
    >(
        yes: S1,
        no: S2,
        start: S3,
        count: S4,
        go: S5,
    ) -> Self {
        ModPoll {
            yes: yes.into().unwrap_or(YES).to_string(),
            no: no.into().unwrap_or(NO).to_string(),
            start: start.into().unwrap_or(START).to_string(),
            count: count.into().unwrap_or(COUNT).to_string(),
            go: go.into().unwrap_or(GO).to_string(),
            ready_polls: Default::default(),
        }
    }

    pub async fn handle_remove_react(
        handler: &Handler,
        ctx: &Context,
        react: &Reaction,
    ) -> anyhow::Result<()> {
        let mut msg = react.message(&ctx.http).await?;
        if Some(&msg.author.id) != handler.self_id.get() {
            return Ok(());
        }
        let (interaction_id, _) = match msg.interaction.as_ref() {
            Some(interaction) if interaction.name == ReadyPoll::NAME => {
                (interaction.id, interaction.user.id)
            }
            _ => return Ok(()),
        };
        let module: &ModPoll = handler.module()?;
        if react.emoji.to_string() != module.yes {
            return Ok(());
        }
        let mut polls = module.ready_polls.lock().await;
        let user_id = react
            .user_id
            .ok_or_else(|| anyhow!("invalid react: missing userId"))?;
        // let guild_id = react
        //     .guild_id
        //     .ok_or_else(|| anyhow!("must be run in a guild"))?;
        // let member = guild_id.member(&ctx.http, user_id).await?;
        // let username = member.display_name();
        let username = format!("<@{user_id}>");
        let ndx = polls.iter().position(|(id, _, _, _)| *id == interaction_id);
        if let Some(ndx) = ndx {
            let usernames = &mut polls[ndx].3;
            if let Some(username_ndx) = usernames.iter().position(|s| s == username.as_str()) {
                usernames.remove(username_ndx);
                let message = build_message(usernames);
                drop(polls);
                msg.edit(&ctx.http, |msg| {
                    msg.content(message)
                        .allowed_mentions(|mentions| mentions.empty_users())
                })
                .await?;
            }
        }
        Ok(())
    }

    pub async fn handle_ready_poll(
        handler: &Handler,
        ctx: &Context,
        react: &Reaction,
    ) -> anyhow::Result<()> {
        let http = &ctx.http;
        let mut msg = react.message(http).await?;
        let (interaction_id, interaction_user) = match msg.interaction.as_ref() {
            Some(interaction) if interaction.name == ReadyPoll::NAME => {
                (interaction.id, interaction.user.id)
            }
            _ => return Ok(()),
        };
        let module: &ModPoll = handler.module()?;
        let mut polls = module.ready_polls.lock().await;
        let user_id = react
            .user_id
            .ok_or_else(|| anyhow!("invalid react: missing userId"))?;
        let ndx = polls.iter().position(|(id, _, _, _)| *id == interaction_id);
        let ndx = match ndx {
            Some(ndx) => ndx,
            None => {
                polls.push_front((interaction_id, None, None, Vec::new()));
                0
            }
        };
        let count = polls[ndx].1.clone();
        let go = polls[ndx].2.clone();
        // let (count, go, mut usernames) = match poll {
        //     Some((_, count, go, usernames)) => (count, go, usernames),
        //     None => (None, None, Vec::new()),
        // };
        // let (ndx, count, go, usernames) = {
        //     if let Some((ndx, (_, count, go, usernames))) = polls
        //         .iter()
        //         .enumerate()
        //         .find(|(_, (id, _, _, _))| *id == interaction.id)
        //     {
        //         let count = count.clone();
        //         let go = go.clone();
        //         (Some(ndx), count, go, usernames.clone())
        //     } else {
        //         (None, None, None, Vec::new())
        //     }
        // };
        if react.emoji.to_string() == module.yes && Some(&user_id) != handler.self_id.get() {
            let username = format!("<@{user_id}>");
            let usernames = &mut polls[ndx].3;
            if !usernames.contains(&username) {
                usernames.push(username.to_string());
            }
            let content = build_message(usernames);
            // release lock before editing message
            drop(polls);
            msg.edit(http, |msg| {
                msg.content(content).allowed_mentions(|m| m.empty_users())
            })
            .await?;
            return Ok(());
        }
        if interaction_user != user_id {
            return Ok(());
        }
        if react.emoji.to_string() != module.start {
            return Ok(());
        }
        polls.remove(ndx);
        drop(polls);
        crabdown(
            handler,
            http,
            msg.channel_id,
            count.as_deref(),
            go.as_deref(),
        )
        .await
    }
}

impl Default for ModPoll {
    fn default() -> Self {
        Self::new(None, None, None, None, None)
    }
}

#[async_trait]
impl Module for ModPoll {
    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Ok(Default::default())
    }

    fn register_commands(&self, store: &mut CommandStore, _completions: &mut CompletionStore) {
        store.register::<ReadyPoll>();
    }
}
