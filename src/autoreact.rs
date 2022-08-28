use std::{collections::HashMap, str::FromStr};

use anyhow::Context as _;
use fallible_iterator::FallibleIterator;
use rusqlite::{params, Connection};
use serenity::{
    model::prelude::{Message, ReactionType},
    prelude::Context,
};

use crate::Handler;

pub struct AutoReact {
    trigger: String,
    emote: ReactionType,
}

fn parse_emote(s: &str) -> anyhow::Result<ReactionType> {
    Ok(ReactionType::from_str(s)?)
}

impl AutoReact {
    fn new(trigger: &str, emote: &str) -> anyhow::Result<AutoReact> {
        let emote = parse_emote(emote)?;
        Ok(AutoReact {
            trigger: trigger.to_string(),
            emote,
        })
    }
}

impl From<(&str, &str)> for AutoReact {
    fn from((trigger, emote): (&str, &str)) -> Self {
        AutoReact::new(trigger, emote).unwrap()
    }
}

pub type ReactsCache = HashMap<u64, Vec<AutoReact>>;

pub async fn new(db: &Connection) -> anyhow::Result<ReactsCache> {
    let cache = {
        db.prepare("SELECT guild_id, trigger, emote FROM autoreact")?
            .query([])?
            .map(|row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .try_fold::<_, anyhow::Error, _>(
                HashMap::<u64, Vec<AutoReact>>::new(),
                |mut cache, (guild_id, trigger, emote): (u64, String, String)| {
                    cache
                        .entry(guild_id)
                        .or_default()
                        .push(AutoReact::new(&trigger, &emote)?);
                    Ok(cache)
                },
            )?
    };
    Ok(cache)
}

impl Handler {
    pub async fn add_autoreact(
        &self,
        guild_id: u64,
        trigger: &str,
        emote: &str,
    ) -> anyhow::Result<()> {
        let parsed = AutoReact::new(trigger, emote)?;
        {
            let db = self.db.lock().await;
            db.execute(
                "INSERT INTO autoreact (guild_id, trigger, emote) VALUES (?1, ?2, ?3)",
                params![guild_id, trigger, emote],
            )?;
        }
        self.reacts_cache
            .write()
            .await
            .entry(guild_id)
            .or_default()
            .push(parsed);
        Ok(())
    }

    pub async fn remove_autoreact(
        &self,
        guild_id: u64,
        trigger: &str,
        emote: &str,
    ) -> anyhow::Result<()> {
        {
            let db = self.db.lock().await;
            db.execute(
                "DELETE FROM autoreact WHERE guild_id = ?1 AND trigger = ?2 AND emote = ?3",
                params![guild_id, trigger, emote],
            )?;
        }
        let emote = parse_emote(emote)?;
        if let Some(reacts) = self.reacts_cache.write().await.get_mut(&guild_id) {
            reacts.retain_mut(|ar| ar.trigger != trigger && ar.emote != emote);
        };
        Ok(())
    }

    pub async fn autocomplete_autoreact(
        &self,
        guild_id: u64,
        trigger: &str,
        emote: &str,
    ) -> anyhow::Result<Vec<(String, String)>> {
        let db = self.db.lock().await;
        let res = db
            .prepare(
                "SELECT trigger, emote FROM autoreact WHERE
                     guild_id = ?1 AND trigger LIKE '%'||?2||'%' AND emote LIKE '%'||?3||'%'
                     LIMIT 25",
            )?
            .query(params![guild_id, trigger, emote])?
            .map(|row| Ok((row.get(0)?, row.get(1)?)))
            .collect()?;
        Ok(res)
    }

    // Add reactions for autoreacts whose trigger match msg
    pub async fn add_reacts(&self, ctx: &Context, msg: Message) -> anyhow::Result<()> {
        let mut lower = msg.content.to_lowercase();
        lower.push_str(
            &msg.embeds
                .iter()
                .flat_map(|e| e.description.as_deref())
                .collect::<String>()
                .to_lowercase(),
        );
        let mut indices = Vec::new();
        let cache = self.reacts_cache.read().await;
        let guild_id = match msg.guild_id {
            Some(id) => id.0,
            None => return Ok(()),
        };
        let reacts = match cache.get(&guild_id) {
            Some(reacts) => reacts,
            None => return Ok(()),
        };
        for (i, react) in reacts.iter().enumerate() {
            if let Some(ndx) = lower.find(&react.trigger) {
                indices.push((ndx, i));
            }
        }
        // sort by trigger position so reacts get added in order
        indices.sort_by_key(|(ndx, _)| *ndx);
        for (_, i) in indices {
            msg.react(&ctx.http, reacts[i].emote.clone())
                .await
                .context("could not add reaction")?;
        }
        Ok(())
    }
}
