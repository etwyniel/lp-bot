use reqwest::{Client, Url};
use serde::Deserialize;
use std::collections::HashMap;

use std::env;
use std::iter::IntoIterator;

const API_ENDPOINT: &str = "http://ws.audioscrobbler.com/2.0/";

pub struct Lastfm {
    client: Client,
    api_key: String,
}

#[derive(Deserialize)]
pub struct Tag {
    pub count: u64,
    pub name: String,
    pub url: String,
}

#[derive(Deserialize)]
pub struct TopTags {
    pub tag: Vec<Tag>,
    #[serde(rename = "@attr")]
    pub attributes: HashMap<String, String>,
}

#[derive(Deserialize)]
pub struct ArtistTopTags {
    pub toptags: TopTags,
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
                .fold(&mut pairs, |pairs, (k, v)| pairs.append_pair(k, &v));
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
        Ok(top_tags.toptags.tag.into_iter().take(5).map(|t| t.name).collect())
    }
}
