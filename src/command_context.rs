use anyhow::anyhow;
use serenity::{
    async_trait,
    client::Context,
    model::{
        channel::{Channel, Message},
        guild::Role,
        interactions::application_command::{
            ApplicationCommandInteraction, ApplicationCommandInteractionDataOption,
            ApplicationCommandInteractionDataOptionValue,
        },
        prelude::InteractionApplicationCommandCallbackDataFlags,
    },
};

use crate::Handler;

pub enum CommandResponse {
    None,
    Public(String),
    Private(String),
}

impl CommandResponse {
    fn to_contents_and_flags(
        self,
    ) -> Option<(String, InteractionApplicationCommandCallbackDataFlags)> {
        Some(match self {
            CommandResponse::None => return None,
            CommandResponse::Public(s) => {
                (s, InteractionApplicationCommandCallbackDataFlags::empty())
            }
            CommandResponse::Private(s) => {
                (s, InteractionApplicationCommandCallbackDataFlags::EPHEMERAL)
            }
        })
    }
}

#[async_trait]
pub trait Responder {
    async fn respond(
        &self,
        contents: CommandResponse,
        role_id: Option<u64>,
    ) -> anyhow::Result<Option<Message>>;
    fn ctx(&self) -> &Context;
    fn handler(&self) -> &Handler;
}

pub struct SlashCommand<'a, 'b> {
    pub handler: &'a Handler,
    pub ctx: &'b Context,
    pub command: &'b ApplicationCommandInteraction,
}

#[async_trait]
impl Responder for SlashCommand<'_, '_> {
    async fn respond(
        &self,
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
                .kind(serenity::model::interactions::InteractionResponseType::ChannelMessageWithSource)
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

    fn ctx(&self) -> &Context {
        self.ctx
    }

    fn handler(&self) -> &Handler {
        self.handler
    }
}

impl<'a, 'b> SlashCommand<'a, 'b> {
    pub fn name(&self) -> &str {
        &self.command.data.name
    }

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

    pub fn str_opt(&self, name: &str) -> Option<String> {
        self.opt(name, |o| {
            if let ApplicationCommandInteractionDataOptionValue::String(s) = o {
                Some(s.clone())
            } else {
                None
            }
        })
    }

    pub fn role_opt(&self, name: &str) -> Option<Role> {
        self.opt(name, |o| {
            if let ApplicationCommandInteractionDataOptionValue::Role(r) = o {
                Some(r.clone())
            } else {
                None
            }
        })
    }

    pub fn bool_opt(&self, name: &str) -> Option<bool> {
        self.opt(name, |o| {
            if let ApplicationCommandInteractionDataOptionValue::Boolean(b) = o {
                Some(*b)
            } else {
                None
            }
        })
    }

    pub fn int_opt(&self, name: &str) -> Option<i64> {
        self.opt(name, |o| {
            if let ApplicationCommandInteractionDataOptionValue::Integer(i) = o {
                Some(*i)
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

    fn ctx(&self) -> &Context {
        self.ctx
    }

    fn handler(&self) -> &Handler {
        self.handler
    }
}

pub fn get_str_opt_ac<'a>(
    options: &'a [ApplicationCommandInteractionDataOption],
    name: &str,
) -> Option<&'a str> {
    options
        .iter()
        .find(|opt| opt.name == name)
        .and_then(|opt| opt.value.as_ref())
        .and_then(|val| val.as_str())
}

pub fn get_focused_option(options: &[ApplicationCommandInteractionDataOption]) -> Option<&str> {
    options
        .iter()
        .find(|opt| opt.focused)
        .map(|opt| opt.name.as_str())
}
