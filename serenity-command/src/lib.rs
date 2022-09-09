use serenity::async_trait;
use serenity::builder::{CreateApplicationCommand, CreateApplicationCommandOption};
use serenity::model::application::interaction::application_command::{
    ApplicationCommandInteraction, CommandData,
};
use serenity::model::prelude::interaction::MessageFlags;
use serenity::prelude::Context;

#[derive(Debug)]
pub enum CommandResponse {
    None,
    Public(String),
    Private(String),
}

impl CommandResponse {
    pub fn to_contents_and_flags(self) -> Option<(String, MessageFlags)> {
        Some(match self {
            CommandResponse::None => return None,
            CommandResponse::Public(s) => (s, MessageFlags::empty()),
            CommandResponse::Private(s) => (s, MessageFlags::EPHEMERAL),
        })
    }
}

pub trait CommandBuilder<'a, T>: From<&'a CommandData> + 'static {
    fn create_extras<E: Fn(&'static str, &mut CreateApplicationCommandOption)>(
        builder: &mut CreateApplicationCommand,
        extras: E,
    ) -> &mut CreateApplicationCommand;
    fn create(builder: &mut CreateApplicationCommand) -> &mut CreateApplicationCommand;
    const NAME: &'static str;
    fn runner() -> Box<dyn CommandRunner<T> + Send + Sync>;
}

#[async_trait]
pub trait CommandRunner<T> {
    async fn run(
        &self,
        data: &T,
        ctx: &Context,
        interaction: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse>;
    fn name(&self) -> &'static str;
}
