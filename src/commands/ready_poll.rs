use std::collections::VecDeque;
use std::str::FromStr;

use serenity::model::id::InteractionId;
use serenity::model::prelude::interaction::application_command::ApplicationCommandInteraction;
use serenity::model::prelude::{Reaction, ReactionType};
use serenity::{async_trait, prelude::Context};
use serenity_command::{BotCommand, CommandResponse};
use serenity_command_derive::Command;

use crate::Handler;

const YES: &str = "<:FeelsGoodCrab:988509541069127780>";
const NO: &str = "<:FeelsBadCrab:988508541499342918>";
const GO: &str = "<a:CrabRave:988508208240922635>";

const MAX_POLLS: usize = 20;

pub type PendingPolls = VecDeque<(InteractionId, Option<String>, Option<String>)>;

#[derive(Command, Debug)]
#[cmd(name = "ready_poll", desc = "Poll to start a listening party")]
pub struct ReadyPoll {
    #[cmd(desc = "Count emote")]
    pub count_emote: Option<String>,
    #[cmd(desc = "Emote Go")]
    pub go_emote: Option<String>,
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
        let http = &ctx.http;
        interaction
            .create_interaction_response(http, |msg| {
                msg.interaction_response_data(|data| data.content("Ready?"))
            })
            .await?;
        let resp = interaction.get_interaction_response(http).await?;
        resp.react(http, ReactionType::from_str(YES)?).await?;
        resp.react(http, ReactionType::from_str(NO)?).await?;
        resp.react(http, ReactionType::from_str(GO)?).await?;
        let mut polls = handler.ready_polls.lock().await;
        while polls.len() >= MAX_POLLS {
            polls.pop_back();
        }
        polls.push_front((interaction.id, self.count_emote, self.go_emote));
        Ok(CommandResponse::None)
    }
}

pub async fn handle_ready_poll(handler: &Handler, react: &Reaction) -> anyhow::Result<()> {
    if react.emoji.to_string() != GO {
        return Ok(());
    }
    let http = handler.http();
    let msg = react.message(http).await?;
    let interaction_id = match msg.interaction.as_ref() {
        None => return Ok(()),
        Some(interaction) => {
            if Some(interaction.user.id) != react.user_id {
                return Ok(());
            }
            interaction.id
        }
    };
    let (count, go) = {
        let mut polls = handler.ready_polls.lock().await;
        if let Some((ndx, (_, count, go))) = polls
            .iter()
            .enumerate()
            .find(|(_, (id, _, _))| *id == interaction_id)
        {
            let count = count.clone();
            let go = go.clone();
            polls.remove(ndx);
            (count, go)
        } else {
            (None, None)
        }
    };
    handler
        .crabdown(http, msg.channel_id, count.as_deref(), go.as_deref())
        .await
}
