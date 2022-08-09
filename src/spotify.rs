use anyhow::{anyhow, bail};
use rspotify::{
    clients::BaseClient,
    model::{AlbumId, Id, PlaylistId, SearchType},
    ClientCredsSpotify, Config, Credentials,
};
use serenity::async_trait;

use crate::album::{Album, AlbumProvider};

const ALBUM_URL_START: &str = "https://open.spotify.com/album/";
const PLAYLIST_URL_START: &str = "https://open.spotify.com/playlist/";

pub struct Spotify {
    client: ClientCredsSpotify,
}

impl Spotify {
    async fn get_album_from_id(&self, id: &str) -> anyhow::Result<Album> {
        let album = self.client.album(&AlbumId::from_id(id)?).await?;
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
            .playlist(&PlaylistId::from_id(id)?, None, None)
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
}

#[async_trait]
impl AlbumProvider for Spotify {
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
            .search(query, &SearchType::Album, None, None, Some(1), None)
            .await?;
        if let rspotify::model::SearchResult::Albums(albums) = res {
            Ok(albums
                .items
                .first()
                .map(|a| Album {
                    name: Some(a.name.clone()),
                    artist: a.artists.first().map(|ar| ar.name.clone()),
                    url: a.id.as_ref().map(|i| i.url()),
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
            .search(query, &SearchType::Album, None, None, Some(10), None)
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

impl Spotify {
    pub async fn new() -> anyhow::Result<Self> {
        let creds = Credentials::from_env().ok_or_else(|| anyhow!("No spotify credentials"))?;
        let config = Config {
            token_refreshing: true,
            ..Default::default()
        };
        let mut spotify = ClientCredsSpotify::with_config(creds, config);

        // Obtaining the access token
        spotify.request_token().await?;
        Ok(Spotify { client: spotify })
    }
}
