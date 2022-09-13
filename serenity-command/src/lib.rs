use serenity::async_trait;
use serenity::builder::{CreateApplicationCommand, CreateApplicationCommandOption};
use serenity::model::application::interaction::application_command::{
    ApplicationCommandInteraction, CommandData,
};
use serenity::model::prelude::interaction::MessageFlags;
use serenity::model::Permissions;
use serenity::prelude::Context;

#[derive(Debug)]
pub enum CommandResponse {
    None,
    Public(String),
    Private(String),
}

#[async_trait]
pub trait BotCommand {
    type Data;
    async fn run(
        self,
        data: &Self::Data,
        ctx: &Context,
        interaction: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse>;

    fn setup_options(_opt_name: &'static str, _opt: &mut CreateApplicationCommandOption) {}

    const PERMISSIONS: Permissions = Permissions::empty();
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

pub trait CommandBuilder<'a>: BotCommand + From<&'a CommandData> + 'static {
    fn create_extras<E: Fn(&'static str, &mut CreateApplicationCommandOption)>(
        builder: &mut CreateApplicationCommand,
        extras: E,
    ) -> &mut CreateApplicationCommand;
    fn create(builder: &mut CreateApplicationCommand) -> &mut CreateApplicationCommand;
    const NAME: &'static str;
    fn runner() -> Box<dyn CommandRunner<Self::Data> + Send + Sync>;
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
    fn register<'a>(
        &self,
        builder: &'a mut CreateApplicationCommand,
    ) -> &'a mut CreateApplicationCommand;
}
