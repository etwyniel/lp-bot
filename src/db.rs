use std::borrow::Cow;
use std::fmt::Write;

use anyhow::bail;
use chrono::{DateTime, NaiveDateTime, Utc};
use fallible_iterator::FallibleIterator;
use rusqlite::{params, types::FromSql, Connection, Error::SqliteFailure, ErrorCode, ToSql};
use serenity::{
    model::{channel::Message, id::MessageId},
    prelude::Mutex,
};

use crate::Handler;

pub struct Quote {
    pub quote_number: u64,
    pub guild_id: u64,
    pub channel_id: u64,
    pub message_id: MessageId,
    pub ts: DateTime<Utc>,
    pub author_id: u64,
    pub author_name: String,
    pub contents: String,
}

pub struct Birthday {
    pub user_id: u64,
    pub day: u8,
    pub month: u8,
    pub year: Option<u16>,
}

impl Handler {
    pub async fn ensure_guild_table(&self, guild_id: u64) -> anyhow::Result<()> {
        let db = self.db.lock().await;
        db.execute(
            "INSERT INTO guild (id) VALUES (?1) ON CONFLICT(id) DO NOTHING",
            [guild_id],
        )?;
        Ok(())
    }

    pub async fn get_guild_field<T: FromSql>(&self, field: &str, guild_id: u64) -> Option<T> {
        let db = self.db.lock().await;
        db.query_row(
            &format!("SELECT {} FROM guild WHERE id = ?1", field),
            [guild_id],
            |row| row.get(0),
        )
        .ok()
    }

    pub async fn set_guild_field<T: ToSql>(
        &self,
        field: &str,
        guild_id: u64,
        value: T,
    ) -> anyhow::Result<()> {
        self.ensure_guild_table(guild_id).await?;
        let db = self.db.lock().await;
        db.execute(
            &format!("UPDATE guild SET {} = ?2 WHERE id = ?1", field),
            params![guild_id, value],
        )?;
        Ok(())
    }

    pub async fn get_create_threads(&self, guild_id: u64) -> bool {
        self.get_guild_field("create_threads", guild_id)
            .await
            .unwrap_or(false)
    }

    pub async fn get_role_id(&self, guild_id: u64) -> Option<u64> {
        self.get_guild_field("role_id", guild_id).await
    }

    pub async fn get_webhook(&self, guild_id: u64) -> Option<String> {
        self.get_guild_field("webhook", guild_id).await
    }

    pub async fn get_pinboard_webhook(&self, guild_id: u64) -> Option<String> {
        self.get_guild_field("pinboard_webhook", guild_id).await
    }

    pub async fn add_quote(&self, guild_id: u64, message: &Message) -> anyhow::Result<Option<u64>> {
        let mut db = self.db.lock().await;
        let tx = db.transaction()?;
        let last_quote: u64 = tx
            .query_row(
                "SELECT quote_number FROM quote WHERE guild_id = ?1 ORDER BY quote_number DESC",
                [guild_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        let channel_id = message.channel_id.0;
        let ts = message.timestamp;
        let author_id = message.author.id.0;
        let author_name = &message.author.name;
        match tx.execute(
            r"INSERT INTO quote (
    guild_id, channel_id, message_id, ts, quote_number,
    author_id, author_name, contents
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                guild_id,
                channel_id,
                message.id.0,
                ts.unix_timestamp(),
                last_quote + 1,
                author_id,
                author_name,
                &message.content
            ],
        ) {
            Err(SqliteFailure(e, _)) if e.code == ErrorCode::ConstraintViolation => {
                return Ok(None); // Quote already exists
            }
            Ok(n) => Ok(Some(n)),
            Err(e) => Err(e),
        }?;
        tx.commit()?;
        Ok(Some(last_quote + 1))
    }

    pub async fn fetch_quote(
        &self,
        guild_id: u64,
        quote_number: u64,
    ) -> anyhow::Result<Option<Quote>> {
        let db = self.db.lock().await;
        let res = db.query_row(
            "SELECT guild_id, channel_id, message_id, ts, author_id, author_name, contents FROM quote
     WHERE guild_id = ?1 AND quote_number = ?2",
            [guild_id, quote_number],
            |row| {
                let dt = NaiveDateTime::from_timestamp(row.get(3)?, 0);
                Ok(Quote {
                    quote_number,
                    guild_id: row.get(0)?,
                    channel_id: row.get(1)?,
                    message_id: MessageId(row.get(2)?),
                    ts: DateTime::<Utc>::from_utc(dt, Utc),
                    author_id: row.get(4)?,
                    author_name: row.get(5)?,
                    contents: row.get(6)?,
                })
            },
        );
        match res {
            Ok(q) => Ok(Some(q)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn get_random_quote(
        &self,
        guild_id: u64,
        user: Option<u64>,
    ) -> anyhow::Result<Option<Quote>> {
        let number = {
            let db = self.db.lock().await;
            let mut stmt = db.prepare(
                "SELECT quote_number FROM quote WHERE guild_id = ?1 AND (?2 IS NULL OR author_id = ?2)",
                )?;
            let numbers: Vec<_> = stmt
                .query(params![guild_id, user])?
                .map(|row| row.get(0))
                .collect()?;
            if numbers.is_empty() {
                bail!("No quotes saved");
            }
            numbers[rand::random::<usize>() % numbers.len()]
        };
        self.fetch_quote(guild_id, number).await
    }

    pub async fn list_quotes(
        &self,
        guild_id: u64,
        like: &str,
    ) -> anyhow::Result<Vec<(u64, String)>> {
        let db = self.db.lock().await;
        let res = db.prepare(
            "SELECT quote_number, contents FROM quote WHERE guild_id = ?1 AND contents LIKE '%'||?2||'%' LIMIT 15",
        )?
            .query(params![guild_id, like])?
            .map(|row| Ok((row.get(0)?, row.get(1)?)))
            .collect()?;
        Ok(res)
    }

    pub async fn add_birthday(
        &self,
        guild_id: u64,
        user_id: u64,
        day: u8,
        month: u8,
        year: Option<u16>,
    ) -> anyhow::Result<()> {
        let db = self.db.lock().await;
        db.execute(
            "INSERT INTO bdays (guild_id, user_id, day, month, year)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(guild_id, user_id) DO UPDATE
                 SET day = ?3, month = ?4, year = ?5
                 WHERE guild_id = ?1 AND user_id = ?2",
            params![guild_id, user_id, day, month, year],
        )?;
        Ok(())
    }

    pub async fn get_bdays(&self, guild_id: u64) -> anyhow::Result<Vec<Birthday>> {
        let db = self.db.lock().await;
        let res = db
            .prepare("SELECT user_id, day, month, year FROM bdays WHERE guild_id = ?1")?
            .query([guild_id])?
            .map(|row| {
                Ok(Birthday {
                    user_id: row.get(0)?,
                    day: row.get(1)?,
                    month: row.get(2)?,
                    year: row.get(3)?,
                })
            })
            .collect()?;
        Ok(res)
    }
}

pub fn get_release_year(db: &Connection, artist: &str, album: &str) -> Option<u64> {
    db.query_row(
        "SELECT year FROM album_cache WHERE artist = ?1 AND album = ?2",
        [artist, album],
        |row| row.get(0),
    )
    .ok()
}

pub async fn set_release_year(
    db: &Mutex<Connection>,
    artist: &str,
    album: &str,
    year: u64,
) -> anyhow::Result<()> {
    let db = db.lock().await;
    db.execute("INSERT INTO album_cache (artist, album, year) VALUES (?1, ?2, ?3) ON CONFLICT(artist, album) DO NOTHING",
    params![artist, album, year])?;
    Ok(())
}

pub fn escape_str(s: &str) -> Cow<'_, str> {
    if !s.contains('\'') {
        return Cow::Borrowed(s);
    }
    Cow::Owned(s.replace('\'', "''"))
}

pub async fn get_release_years<'a, I: IntoIterator<Item = (&'a str, &'a str, usize)>>(
    db: &Mutex<Connection>,
    albums: I,
) -> anyhow::Result<Vec<(usize, u64)>> {
    let mut query = "WITH albums_in(artist, album, pos) AS(VALUES".to_string();
    albums.into_iter().enumerate().for_each(|(i, ab)| {
        if i > 0 {
            query.push(',');
        }
        write!(
            &mut query,
            "('{}', '{}', {})",
            escape_str(ab.0),
            escape_str(ab.1),
            ab.2
        )
        .unwrap();
    });
    query.push_str(
        ")
        SELECT albums_in.pos, album_cache.year
        FROM album_cache JOIN albums_in
        ON albums_in.artist = album_cache.artist
        AND albums_in.album = album_cache.album",
    );
    let db = db.lock().await;
    let mut stmt = db.prepare(&query)?;
    let res = stmt
        .query([])?
        .map(|row| Ok((row.get(0)?, row.get(1)?)))
        .collect()
        .map_err(anyhow::Error::from);
    res
}

pub fn init() -> anyhow::Result<Connection> {
    let conn = Connection::open("lpbot.sqlite")?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS guild (
            id INTEGER PRIMARY KEY,
            role_id INTEGER,
            create_threads BOOLEAN NOT NULL DEFAULT(TRUE),
            webhook STRING,
            pinboard_webhook STRING
        )",
        [],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS quote (
            guild_id INTEGER,
            channel_id INTEGER,
            message_id INTEGER,
            ts INTEGER,
            quote_number INTEGER,
            author_id INTEGER,
            author_name STRING,
            contents STRING,
            UNIQUE(guild_id, quote_number),
            UNIQUE(guild_id, message_id)
        )",
        [],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS bdays (
            guild_id INTEGER NOT NULL,
            user_id INTEGER NOT NULL,
            day INTEGER NOT NULL,
            month INTEGER NOT NULL,
            year INTEGER,
            UNIQUE(guild_id, user_id)
        )",
        [],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS autoreact (
            guild_id INTEGER NOT NULL,
            trigger STRING NOT NULL,
            emote STRING NOT NULL
        )",
        [],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS album_cache (
            artist STRING NOT NULL,
            album STRING NOT NULL,
            year INTEGER NOT NULL,
            UNIQUE(artist, album)
        )",
        [],
    )?;

    Ok(conn)
}
