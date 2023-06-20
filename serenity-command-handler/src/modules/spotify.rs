use std::collections::HashSet;

use crate::{Module, ModuleMap};
use anyhow::{anyhow, bail};
use rspotify::{
    clients::{BaseClient, OAuthClient},
    model::{AlbumId, FullTrack, Id, PlaylistId, SearchType, SimplifiedArtist, TrackId},
    AuthCodeSpotify, ClientCredsSpotify, Config, Credentials,
};
use serenity::async_trait;

use crate::album::{Album, AlbumProvider};

const ALBUM_URL_START: &str = "https://open.spotify.com/album/";
const PLAYLIST_URL_START: &str = "https://open.spotify.com/playlist/";
const TRACK_URL_START: &str = "https://open.spotify.com/track/";

const CACHE_PATH: &str = "rspotify_cache";

pub struct Spotify<C: BaseClient> {
    // client: ClientCredsSpotify,
    pub client: C,
}

pub type SpotifyOAuth = Spotify<AuthCodeSpotify>;

impl<C: BaseClient> Spotify<C> {
    async fn get_album_from_id(&self, id: &str) -> anyhow::Result<Album> {
        let album = self.client.album(AlbumId::from_id(id)?).await?;
        let name = album.name.clone();
        let artist = album
            .artists
            .iter()
            .map(|a| a.name.as_ref())
            .collect::<Vec<_>>()
            .join(", ");
        let genres = album.genres.clone();
        let release_date = Some(album.release_date);
        Ok(Album {
            name: Some(name),
            artist: Some(artist),
            genres,
            release_date,
            url: Some(album.id.url()),
            ..Default::default()
        })
    }

    async fn get_playlist_from_id(&self, id: &str) -> anyhow::Result<Album> {
        let playlist = self
            .client
            .playlist(PlaylistId::from_id(id)?, None, None)
            .await?;
        let name = playlist.name.clone();
        let artist = playlist.owner.display_name;
        Ok(Album {
            name: Some(name),
            artist,
            url: Some(playlist.id.url()),
            ..Default::default()
        })
    }

    pub async fn get_song_from_id(&self, id: &str) -> anyhow::Result<FullTrack> {
        Ok(self.client.track(TrackId::from_id(id)?).await?)
    }

    pub async fn get_song_from_url(&self, url: &str) -> anyhow::Result<FullTrack> {
        if let Some(id) = url.strip_prefix(TRACK_URL_START) {
            self.get_song_from_id(id.split('?').next().unwrap()).await
        } else {
            bail!("Invalid spotify url")
        }
    }

    pub fn artists_to_string(artists: &[SimplifiedArtist]) -> String {
        artists
            .iter()
            .map(|a| a.name.as_ref())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn sanitize_string(s: &str) -> String {
    s.chars()
        .filter(|&c| !r#""'+()[]"#.contains(c))
        .take(30)
        .collect()
}

#[async_trait]
impl<C: BaseClient> AlbumProvider for Spotify<C> {
    fn id(&self) -> &'static str {
        "spotify"
    }

    async fn get_from_url(&self, url: &str) -> anyhow::Result<Album> {
        if let Some(id) = url.strip_prefix(ALBUM_URL_START) {
            self.get_album_from_id(id.split('?').next().unwrap()).await
        } else if let Some(id) = url.strip_prefix(PLAYLIST_URL_START) {
            self.get_playlist_from_id(id.split('?').next().unwrap())
                .await
        } else {
            bail!("Invalid spotify url")
        }
    }

    fn url_matches(&self, url: &str) -> bool {
        url.starts_with(ALBUM_URL_START) || url.starts_with(PLAYLIST_URL_START)
    }

    async fn query_album(&self, query: &str) -> anyhow::Result<Album> {
        let res = self
            .client
            .search(query, SearchType::Album, None, None, Some(1), None)
            .await?;
        if let rspotify::model::SearchResult::Albums(albums) = res {
            Ok(albums
                .items
                .first()
                .map(|a| Album {
                    name: Some(a.name.clone()),
                    artist: a.artists.first().map(|ar| ar.name.clone()),
                    url: a.id.as_ref().map(|i| i.url()),
                    release_date: a.release_date.clone(),
                    ..Default::default()
                })
                .ok_or_else(|| anyhow!("Not found"))?)
        } else {
            Err(anyhow!("Not an album"))
        }
    }

    async fn query_albums(&self, query: &str) -> anyhow::Result<Vec<(String, String)>> {
        let res = self
            .client
            .search(query, SearchType::Album, None, None, Some(10), None)
            .await?;
        if let rspotify::model::SearchResult::Albums(albums) = res {
            Ok(albums
                .items
                .into_iter()
                .map(|a| {
                    (
                        format!(
                            "{} - {}",
                            a.artists
                                .into_iter()
                                .next()
                                .map(|ar| ar.name)
                                .unwrap_or_default(),
                            a.name,
                        ),
                        a.id.map(|id| id.url()).unwrap_or_default(),
                    )
                })
                .collect())
        } else {
            Err(anyhow!("Not an album"))
        }
    }
}

impl<C: BaseClient> Spotify<C> {
    pub async fn get_album(&self, artist: &str, name: &str) -> anyhow::Result<Option<Album>> {
        let query = format!(
            r#"album:"{}" artist:"{}""#,
            &sanitize_string(name),
            &sanitize_string(artist)
        );
        let res = self
            .client
            .search(&query, SearchType::Album, None, None, Some(5), None)
            .await?;
        if let rspotify::model::SearchResult::Albums(albums) = res {
            let album = albums
                .items
                .iter()
                .find(|ab| ab.name == name)
                .or_else(|| albums.items.first());
            Ok(album.map(|a| Album {
                name: Some(a.name.clone()),
                artist: a.artists.first().map(|ar| ar.name.clone()),
                url: a.id.as_ref().map(|i| i.url()),
                release_date: a.release_date.clone(),
                ..Default::default()
            }))
        } else {
            Err(anyhow!("Not an album"))
        }
    }

    pub async fn query_songs(&self, query: &str) -> anyhow::Result<Vec<(String, String)>> {
        let res = self
            .client
            .search(query, SearchType::Track, None, None, Some(10), None)
            .await?;
        if let rspotify::model::SearchResult::Tracks(songs) = res {
            Ok(songs
                .items
                .into_iter()
                .map(|a| {
                    (
                        format!(
                            "{} - {}",
                            a.artists
                                .into_iter()
                                .next()
                                .map(|ar| ar.name)
                                .unwrap_or_default(),
                            a.name,
                        ),
                        a.id.map(|id| id.url()).unwrap_or_default(),
                    )
                })
                .collect())
        } else {
            Err(anyhow!("Not an album"))
        }
    }
}

impl Spotify<ClientCredsSpotify> {
    pub async fn new() -> anyhow::Result<Self> {
        let creds = Credentials::from_env().ok_or_else(|| anyhow!("No spotify credentials"))?;
        let config = Config {
            token_refreshing: true,
            ..Default::default()
        };
        let spotify = ClientCredsSpotify::with_config(creds, config);

        // Obtaining the access token
        spotify.request_token().await?;
        Ok(Spotify { client: spotify })
    }
}

impl Spotify<AuthCodeSpotify> {
    pub async fn new_auth_code(scopes: HashSet<String>) -> anyhow::Result<Self> {
        let creds = Credentials::from_env().ok_or_else(|| anyhow!("No spotify credentials"))?;
        let oauth =
            rspotify::OAuth::from_env(scopes).ok_or_else(|| anyhow!("No oauth information"))?;
        let mut client = AuthCodeSpotify::new(creds, oauth);
        client.config.token_cached = true;
        client.config.cache_path = CACHE_PATH.into();
        // let prev_token = Token::from_cache(CACHE_PATH).ok();
        // if let Some(tok) = prev_token {
        //     *client.token.lock().await.unwrap() = Some(tok);
        // } else {
        let url = client.get_authorize_url(false)?;
        // eprintln!("url: {url}");
        // }
        client.prompt_for_token(&url).await?;
        Ok(Spotify { client })
    }
}

#[async_trait]
impl Module for Spotify<ClientCredsSpotify> {
    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Spotify::new().await
    }
}

#[async_trait]
impl Module for Spotify<AuthCodeSpotify> {
    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Err(anyhow!(
            "Must be initialized with new_auth_code and added using with_module"
        ))
    }
}
