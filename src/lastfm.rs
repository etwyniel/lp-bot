use anyhow::Context as _;
use chrono::{DateTime, Datelike, Utc};
use futures::{StreamExt, TryStreamExt};
use image::imageops::FilterType;
use image::io::Reader;
use image::{GenericImage, ImageOutputFormat, RgbaImage};
use regex::Regex;
use reqwest::{Client, Method, Url};
use rspotify::ClientError;
use rusqlite::Connection;
use serde::Deserialize;
use serenity::async_trait;
use serenity::model::prelude::interaction::application_command::ApplicationCommandInteraction;
use serenity::model::prelude::interaction::InteractionResponseType;
use serenity::model::prelude::AttachmentType;
use serenity::prelude::{Context, Mutex};
use serenity_command::{BotCommand, CommandResponse};

use std::borrow::Cow;
use std::collections::HashMap;
use std::env;
use std::io::Cursor;
use std::iter::IntoIterator;
use std::sync::Arc;
use std::time::Duration;

use crate::spotify::Spotify;
use crate::Handler;
use serenity_command_derive::Command;

const API_ENDPOINT: &str = "http://ws.audioscrobbler.com/2.0/";

const CHART_SQUARE_SIZE: u32 = 300;

pub struct Lastfm {
    client: Client,
    api_key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Tag {
    pub count: u64,
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopTags {
    pub tag: Vec<Tag>,
    #[serde(rename = "@attr")]
    pub attributes: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtistTopTags {
    pub toptags: TopTags,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Date {
    pub uts: String,
    #[serde(rename = "#text")]
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtistId {
    pub mbid: String,
    #[serde(rename = "#text")]
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlbumId {
    pub mbid: String,
    #[serde(rename = "#text")]
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Image {
    pub size: String,
    #[serde(rename = "#text")]
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecentTrackAttrs {
    pub nowplaying: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Track {
    pub name: String,
    pub url: String,
    pub mbid: String,
    pub date: Option<Date>,
    pub artist: ArtistId,
    pub album: AlbumId,
    pub image: Vec<Image>,
    #[serde(rename = "@attr")]
    pub attr: Option<RecentTrackAttrs>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecentTracksAttrs {
    pub user: String,
    #[serde(rename = "totalPages")]
    pub total_pages: String,
    pub total: String,
    pub page: String,
    #[serde(rename = "perPage")]
    pub per_page: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecentTracks {
    pub track: Vec<Track>,
    #[serde(rename = "@attr")]
    pub attr: RecentTracksAttrs,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecentTracksResp {
    pub recenttracks: RecentTracks,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Album {
    pub name: String,
    pub url: String,
    pub mbid: String,
    pub playcount: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtistShort {
    pub url: String,
    pub name: String,
    pub mbid: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopAlbum {
    pub name: String,
    pub mbid: String,
    pub url: String,
    pub artist: ArtistShort,
    pub image: Vec<Image>,
    pub playcount: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopAlbumsAttr {
    pub user: String,
    #[serde(rename = "totalPages")]
    pub total_pages: String,
    pub page: String,
    pub total: String,
    #[serde(rename = "perPage")]
    pub per_page: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopAlbums {
    pub album: Vec<TopAlbum>,
    #[serde(rename = "@attr")]
    pub attr: TopAlbumsAttr,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopAlbumsResp {
    pub topalbums: TopAlbums,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MbReleaseInfo {
    pub id: String,
    pub title: String,
    pub date: String,
}

#[derive(Command, Debug)]
#[cmd(name = "aoty", desc = "Get your albums of the year")]
pub struct GetAotys {
    #[cmd(desc = "Last.fm username")]
    pub username: String,
    pub year: Option<i64>,
    #[cmd(desc = "Skip albums without album art")]
    pub skip: Option<bool>,
}

#[async_trait]
impl BotCommand for GetAotys {
    type Data = Handler;

    async fn run(
        self,
        handler: &Handler,
        ctx: &Context,
        opts: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        opts.create_interaction_response(&ctx.http, |r| {
            r.kind(InteractionResponseType::DeferredChannelMessageWithSource)
        })
        .await?;
        if let Err(e) = self.get_aotys(handler, opts).await {
            eprintln!("get aotys failed: {:?}", &e);
            opts.create_followup_message(&ctx.http, |resp| resp.content(e.to_string()))
                .await?;
        }
        Ok(CommandResponse::None)
    }
}

impl GetAotys {
    async fn get_aotys(
        self,
        handler: &Handler,
        opts: &ApplicationCommandInteraction,
    ) -> anyhow::Result<()> {
        let lastfm = Arc::clone(&handler.lastfm);
        let db = Arc::clone(&handler.db);
        let year = self
            .year
            .map(|yr| yr as u64)
            .unwrap_or_else(|| Utc::today().year() as u64);
        let mut aotys = lastfm
            .get_albums_of_the_year(db, Arc::clone(&handler.spotify), &self.username, year)
            .await?;
        let http = handler.http();
        if aotys.is_empty() {
            opts.create_followup_message(http, |msg| {
                msg.content(format!(
                    "No {} albums found for user {}",
                    year, &self.username
                ))
            })
            .await?;
            return Ok(());
        }
        aotys.truncate(25);
        let image = create_aoty_chart(&aotys, self.skip.unwrap_or(false)).await?;
        let mut content = format!("**Top albums of {} for {}**", year, &self.username);
        aotys
            .iter()
            .map(|ab| {
                format!(
                    "{} - {} ({} plays)",
                    &ab.artist.name, &ab.name, &ab.playcount
                )
            })
            .for_each(|line| {
                content.push('\n');
                content.push_str(&line);
            });
        opts.create_followup_message(http, |msg| {
            msg.content(content).add_file(AttachmentType::Bytes {
                data: Cow::Owned(image),
                filename: format!("{}_aoty_{}.png", &self.username, year),
            })
        })
        .await?;
        Ok(())
    }
}

pub async fn create_aoty_chart(albums: &[TopAlbum], skip: bool) -> anyhow::Result<Vec<u8>> {
    let n = (albums.len() as f32).sqrt().ceil() as u32;
    let len = n * CHART_SQUARE_SIZE;
    let mut height = n;
    while (height - 1) * n >= albums.len() as u32 {
        height -= 1;
    }
    let mut out = RgbaImage::new(len, height * CHART_SQUARE_SIZE);
    let mut futures = Vec::new();
    let mut offset = 0;
    for (mut i, album) in albums.iter().enumerate() {
        let image_url = match album.image.iter().find(|img| &img.size == "large") {
            Some(img) => img.url.clone(),
            None => {
                offset += 1;
                continue;
            }
        };
        futures.push(tokio::spawn(async move {
            let reader = match reqwest::get(&image_url).await {
                Ok(resp) => Reader::new(Cursor::new(
                    resp.bytes().await.context("Error getting album cover")?,
                )),
                Err(_) => return Ok((i, None)),
            };
            let img = reader.with_guessed_format()?.decode()?.resize(
                CHART_SQUARE_SIZE,
                CHART_SQUARE_SIZE,
                FilterType::Triangle,
            );
            if skip {
                i -= offset;
            }
            Ok::<_, anyhow::Error>((i - offset, Some(img)))
        }))
    }
    offset = 0;
    for fut in futures {
        let (mut i, img) = match fut.await? {
            Ok((i, Some(img))) => (i, img),
            _ => {
                offset += 1;
                continue;
            }
        };
        if skip {
            i -= offset;
        }
        let y = (i as u32 / n) * CHART_SQUARE_SIZE;
        let x = (i as u32 % n) * CHART_SQUARE_SIZE;
        out.copy_from(&img, x, y)?;
    }
    let buf = Vec::new();
    let mut writer = Cursor::new(buf);
    out.write_to(&mut writer, ImageOutputFormat::Png)?;
    Ok(writer.into_inner())
}

async fn retrieve_release_year(url: &str) -> anyhow::Result<Option<u64>> {
    let client = reqwest::Client::new();
    let resp = client
        .request(Method::GET, url)
        .header("accept", "text/html")
        .header("user-agent", "lpbot (0.1.0)")
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    let re = Regex::new(r"(?m)<dt.+>Release Date</dt>\s*<dd[^>]+>([^<]+)<").unwrap();
    if let Some(cap) = re.captures(&text) {
        cap.get(1)
            .unwrap()
            .as_str()
            .rsplit(' ')
            .next()
            .unwrap()
            .parse()
            .map_err(anyhow::Error::from)
            .map(Some)
    } else {
        eprintln!("Resp ({})", status);
        Ok(None)
    }
}

impl Lastfm {
    pub fn new() -> Self {
        let api_key = env::var("LFM_API_KEY").unwrap();
        let client = Client::new();
        Lastfm { client, api_key }
    }

    async fn query<'a, T, I: IntoIterator<Item = (&'static str, &'a str)>>(
        &self,
        method: &str,
        params: I,
    ) -> anyhow::Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let mut url = Url::parse(API_ENDPOINT)?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs
                .append_pair("method", method)
                .append_pair("api_key", &self.api_key)
                .append_pair("format", "json");
            params
                .into_iter()
                .fold(&mut pairs, |pairs, (k, v)| pairs.append_pair(k, v));
        }
        self.client
            .get(url)
            .send()
            .await?
            .json()
            .await
            .map_err(anyhow::Error::from)
    }

    pub async fn artist_top_tags(&self, artist: &str) -> anyhow::Result<Vec<String>> {
        let top_tags: ArtistTopTags = self
            .query("artist.getTopTags", [("artist", artist)])
            .await?;
        Ok(top_tags
            .toptags
            .tag
            .into_iter()
            .take(5)
            .map(|t| t.name)
            .collect())
    }

    pub async fn get_recent_tracks(
        &self,
        user: &str,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        limit: Option<u64>,
        page: Option<u64>,
    ) -> anyhow::Result<RecentTracks> {
        let mut params: Vec<(&'static str, &str)> = vec![("user", user)];

        let from_s = from.map(|from| from.timestamp().to_string());
        if let Some(from) = from_s.as_deref() {
            params.push(("from", from));
        }
        let to_s = to.map(|to| to.timestamp().to_string());
        if let Some(to) = to_s.as_deref() {
            params.push(("to", to));
        }
        let limit_s = limit.map(|limit| limit.to_string());
        if let Some(limit) = limit_s.as_deref() {
            params.push(("limit", limit));
        }
        let page_s = page.map(|page| page.to_string());
        if let Some(page) = page_s.as_deref() {
            params.push(("page", page));
        }

        let recent_tracks: RecentTracksResp = self.query("user.getrecenttracks", params).await?;
        Ok(recent_tracks.recenttracks)
    }

    pub async fn get_top_albums(&self, user: &str, page: Option<u64>) -> anyhow::Result<TopAlbums> {
        let mut params: Vec<(&'static str, &str)> = vec![("user", user), ("limit", "200")];

        let page_s = page.map(|p| p.to_string());
        if let Some(page) = page_s.as_deref() {
            params.push(("page", page));
        }

        let top_albums: TopAlbumsResp = self.query("user.gettopalbums", params).await?;
        Ok(top_albums.topalbums)
    }

    pub async fn get_albums_of_the_year(
        self: Arc<Self>,
        db: Arc<Mutex<Connection>>,
        spotify: Arc<Spotify>,
        user: &str,
        year: u64,
    ) -> anyhow::Result<Vec<TopAlbum>> {
        let mut aotys = Vec::<TopAlbum>::new();
        let mut page = 1;
        let mut top_albums_fut = Some(tokio::spawn({
            let user = user.to_string();
            let lastfm = Arc::clone(&self);
            let page = page;
            async move { lastfm.get_top_albums(&user, Some(page)).await }
        }));
        loop {
            eprintln!("Querying page {}", page);
            let top_albums = match top_albums_fut.take() {
                Some(fut) => fut.await?.context("Error getting top albums")?,
                None => break,
            };
            let last_plays: Option<u64> = top_albums
                .album
                .last()
                .map(|ab| ab.playcount.parse().unwrap());
            let total_pages = top_albums
                .attr
                .total_pages
                .parse::<u64>()
                .context("Invalid response from last.fm")?;
            if page < total_pages && last_plays.unwrap_or_default() >= 10 {
                page += 1;
                top_albums_fut = Some(tokio::spawn({
                    let user = user.to_string();
                    let lastfm = Arc::clone(&self);
                    let page = page;
                    async move { lastfm.get_top_albums(&user, Some(page)).await }
                }));
            }
            let tuples = top_albums
                .album
                .iter()
                .enumerate()
                .map(|(i, ab)| (ab.artist.name.as_str(), ab.name.as_str(), i));
            let res = crate::db::get_release_years(&db, tuples).await?;
            let mut years: Vec<Option<u64>> = vec![None; top_albums.album.len()];
            res.into_iter().for_each(|(i, year)| years[i] = Some(year));
            let fetches = futures::stream::iter(
                top_albums
                    .album
                    .iter()
                    .cloned()
                    .zip(years.into_iter())
                    .enumerate()
                    .filter(|(_, (ab, yr))| {
                        ab.playcount.parse::<u64>().unwrap() > 10
                            && yr.map(|yr| yr == year).unwrap_or(true)
                    })
                    .map(|(i, (ab, yr))| {
                        tokio::spawn({
                            let db = Arc::clone(&db);
                            let spotify = Arc::clone(&spotify);
                            let name = ab.name.clone();
                            let artist = ab.artist.name.clone();
                            let album = ab.name.clone();
                            let url = ab.url;
                            async move {
                                // Backoff loop
                                if let Some(year) = yr {
                                    return Ok((i, Some(year)));
                                }
                                let lastfm_release_year = retrieve_release_year(&url).await;
                                match lastfm_release_year {
                                    Ok(Some(year)) => {
                                        crate::db::set_release_year(&db, &artist, &album, year)
                                            .await?;
                                        return Ok((i, Some(year)));
                                    }
                                    Err(e) => eprintln!(
                                        "Error getting release year from lastfm: {}",
                                        e
                                    ),
                                    _ => (),
                                }
                                eprintln!("Release date for {} - {} not found in cache or on lastfm, trying spotify", &artist, &album);
                                loop {
                                    match spotify.get_album(&artist, &album).await {
                                        Ok(Some(crate::album::Album {
                                            release_date: Some(date),
                                            ..
                                        })) => {
                                            let year =
                                                date.split('-').next().unwrap().parse().unwrap();
                                            crate::db::set_release_year(&db, &artist, &name, year)
                                                .await?;
                                            break Ok((i, Some(year)));
                                        }
                                        Ok(_) => {
                                            eprintln!("No release year found for {}", &url);
                                            break Ok((i, None));
                                        }
                                        Err(e) => {
                                            let mut retry = false;
                                            for err in e.chain() {
                                                if let Some(ClientError::Http(http_err)) =
                                                    err.downcast_ref()
                                                {
                                                    if let rspotify_http::HttpError::StatusCode(
                                                        code,
                                                    ) = http_err.as_ref()
                                                    {
                                                        if code.status() == 429 {
                                                            retry = true;
                                                        }
                                                    }
                                                }
                                            }
                                            if &e.to_string() == "Not found" {
                                                break Ok((i, None));
                                            }
                                            if !retry {
                                                eprintln!(
                                                    "query {} {} failed: {:?}",
                                                    &artist, &name, &e
                                                );
                                                break Err(e);
                                            }
                                            // Wait before retrying
                                            tokio::time::sleep(Duration::from_secs(5)).await;
                                        }
                                    }
                                }
                            }
                        })
                    }),
            )
            .buffer_unordered(10)
            .map(|res| match res {
                Ok(inner) => inner,
                Err(e) => Err(anyhow::Error::from(e)),
            })
            .map(|res| match res {
                Ok((i, yr)) => Ok((i, yr == Some(year))),
                Err(e) => Err(e),
            })
            .try_collect::<HashMap<usize, bool>>();
            let album_infos = fetches.await?;
            aotys.extend(
                top_albums
                    .album
                    .into_iter()
                    .enumerate()
                    .filter(|(i, _)| album_infos.get(i).copied() == Some(true))
                    .map(|(_, ab)| ab),
            );
            if top_albums_fut.is_none() || aotys.len() >= 25 {
                break;
            }
        }
        Ok(aotys)
    }
}
