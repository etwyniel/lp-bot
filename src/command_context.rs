use anyhow::anyhow;
use serenity::{
    async_trait,
    client::Context,
    model::{
        application::interaction::application_command::{
            CommandDataOption, CommandDataOptionValue,
        },
        channel::{Channel, Message},
        prelude::interaction::application_command::ApplicationCommandInteraction,
    },
};

use crate::Handler;

use serenity_command::CommandResponse;

#[async_trait]
pub trait Responder {
    async fn respond(
        &self,
        ctx: &Context,
        contents: CommandResponse,
        role_id: Option<u64>,
    ) -> anyhow::Result<Option<Message>>;
}

pub struct SlashCommand<'a, 'b> {
    pub handler: &'a Handler,
    pub ctx: &'b Context,
    pub command: &'b ApplicationCommandInteraction,
}

#[async_trait]
impl Responder for ApplicationCommandInteraction {
    async fn respond(
        &self,
        ctx: &Context,
        contents: CommandResponse,
        role_id: Option<u64>,
    ) -> anyhow::Result<Option<Message>> {
        let (contents, flags) = match contents.to_contents_and_flags() {
            None => return Ok(None),
            Some(c) => c,
        };
        self
            .create_interaction_response(&ctx.http, |resp|
                resp
                .kind(serenity::model::application::interaction::InteractionResponseType::ChannelMessageWithSource)
                .interaction_response_data(|message|
                    message
                    .content(&contents)
                    .flags(flags)
                    .allowed_mentions(|mentions| mentions.roles(role_id))
                )
            ).await?;
        self
            .get_interaction_response(&ctx.http)
            .await
            .map_err(anyhow::Error::from)
            .map(Some)
    }
}

#[async_trait]
impl Responder for SlashCommand<'_, '_> {
    async fn respond(
        &self,
        _ctx: &Context,
        contents: CommandResponse,
        role_id: Option<u64>,
    ) -> anyhow::Result<Option<Message>> {
        let (contents, flags) = match contents.to_contents_and_flags() {
            None => return Ok(None),
            Some(c) => c,
        };
        self.command
            .create_interaction_response(&self.ctx.http, |resp|
                resp
                .kind(serenity::model::application::interaction::InteractionResponseType::ChannelMessageWithSource)
                .interaction_response_data(|message|
                    message
                    .content(&contents)
                    .flags(flags)
                    .allowed_mentions(|mentions| mentions.roles(role_id))
                )
            ).await?;
        self.command
            .get_interaction_response(&self.ctx.http)
            .await
            .map_err(anyhow::Error::from)
            .map(Some)
    }
}

impl<'a, 'b> SlashCommand<'a, 'b> {
    pub fn name(&self) -> &str {
        &self.command.data.name
    }

    fn opt<T>(
        &self,
        name: &str,
        getter: impl FnOnce(&CommandDataOptionValue) -> Option<T>,
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

    pub fn str_opt(&self, name: &str) -> Option<String> {
        self.opt(name, |o| {
            if let CommandDataOptionValue::String(s) = o {
                Some(s.clone())
            } else {
                None
            }
        })
    }

    pub fn number_opt(&self, name: &str) -> Option<f64> {
        self.opt(name, |o| {
            if let CommandDataOptionValue::Number(n) = o {
                Some(*n)
            } else {
                None
            }
        })
    }
}

pub struct TextCommand<'a, 'b> {
    pub handler: &'a Handler,
    pub ctx: &'b Context,
    pub message: &'b Message,
}

#[async_trait]
impl Responder for TextCommand<'_, '_> {
    async fn respond(
        &self,
        _ctx: &Context,
        contents: CommandResponse,
        role_id: Option<u64>,
    ) -> anyhow::Result<Option<Message>> {
        let (contents, _) = match contents.to_contents_and_flags() {
            None => return Ok(None),
            Some(c) => c,
        };
        let channel = match self.message.channel(&self.ctx.http).await? {
            Channel::Guild(c) => c,
            _ => return Err(anyhow!("Invalid channel")),
        };
        channel
            .send_message(&self.ctx.http, |msg| {
                msg.content(&contents)
                    .allowed_mentions(|mentions| mentions.roles(role_id))
            })
            .await
            .map_err(anyhow::Error::from)
            .map(Some)
    }
}

pub fn get_str_opt_ac<'a>(options: &'a [CommandDataOption], name: &str) -> Option<&'a str> {
    options
        .iter()
        .find(|opt| opt.name == name)
        .and_then(|opt| opt.value.as_ref())
        .and_then(|val| val.as_str())
}

#[allow(unused)]
pub fn get_int_opt_ac(options: &[CommandDataOption], name: &str) -> Option<i64> {
    options
        .iter()
        .find(|opt| opt.name == name)?
        .value
        .as_ref()?
        .as_i64()
}

pub fn get_focused_option(options: &[CommandDataOption]) -> Option<&str> {
    options
        .iter()
        .find(|opt| opt.focused)
        .map(|opt| opt.name.as_str())
}
