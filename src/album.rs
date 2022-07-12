use serenity::async_trait;

#[derive(Debug, Default)]
pub struct Album {
    pub name: Option<String>,
    pub artist: Option<String>,
    pub genres: Vec<String>,
    pub release_date: Option<String>,
    pub url: Option<String>,
}

#[async_trait]
pub trait AlbumProvider: Send + Sync {
    fn url_matches(&self, _url: &str) -> bool;

    fn id(&self) -> &'static str;

    async fn get_from_url(&self, url: &str) -> anyhow::Result<Album>;

    async fn query_album(&self, _q: &str) -> anyhow::Result<Album>;
}
