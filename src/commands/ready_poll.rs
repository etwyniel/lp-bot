use std::str::FromStr;

use serenity::model::prelude::interaction::application_command::ApplicationCommandInteraction;
use serenity::model::prelude::{Reaction, ReactionType};
use serenity::{async_trait, prelude::Context};
use serenity_command::{BotCommand, CommandResponse};
use serenity_command_derive::Command;

use crate::Handler;

const YES: &str = "<:FeelsGoodCrab:988509541069127780>";
const NO: &str = "<:FeelsBadCrab:988508541499342918>";
const GO: &str = "<a:CrabRave:988508208240922635>";

#[derive(Command, Debug)]
#[cmd(name = "ready_poll", desc = "Poll to start a listening party")]
pub struct ReadyPoll {}

#[async_trait]
impl BotCommand for ReadyPoll {
    type Data = Handler;

    async fn run(
        self,
        _data: &Handler,
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
        Ok(CommandResponse::None)
    }
}

pub async fn handle_ready_poll(handler: &Handler, react: &Reaction) -> anyhow::Result<()> {
    if react.emoji.to_string() != GO {
        return Ok(());
    }
    let http = handler.http();
    let msg = react.message(http).await?;
    match msg.interaction.as_ref() {
        None => return Ok(()),
        Some(interaction) => {
            if Some(interaction.user.id) != react.user_id {
                return Ok(());
            }
        }
    }
    handler.crabdown(http, msg.channel_id).await
}
