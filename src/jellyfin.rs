use bytes::Buf;
use serde::Deserialize;

pub struct JellyfinClient {
    api_token: String,
    base_url: String,
    client: reqwest::Client,
}

#[derive(Deserialize)]
pub struct Audio {
    #[serde(rename(deserialize = "Id"))]
    pub id: String,
    #[serde(rename(deserialize = "Name"))]
    pub name: String,
    #[serde(rename(deserialize = "Artists"))]
    pub artists: Vec<String>,
}

#[derive(Deserialize)]
pub struct View {
    #[serde(rename(deserialize = "Name"))]
    pub name: String,
    #[serde(rename(deserialize = "Id"))]
    pub id: String,
    #[serde(rename(deserialize = "CollectionType"))]
    pub collection_type: String,
}

#[derive(Deserialize)]
pub struct User {
    #[serde(rename(deserialize = "Name"))]
    pub name: String,
    #[serde(rename(deserialize = "Id"))]
    pub id: String,
    #[serde(rename(deserialize = "Policy"))]
    pub policy: UserPolicy,
}

#[derive(Deserialize)]
pub struct UserPolicy {
    #[serde(rename(deserialize = "IsAdministrator"))]
    pub is_administrator: bool,
}

impl JellyfinClient {
    pub fn new(base_url: String, api_token: String) -> Self {
        Self {
            base_url,
            api_token,
            client: reqwest::Client::new(),
        }
    }

    pub async fn users(&self) -> anyhow::Result<Vec<User>> {
        let url = format!("{}/Users", self.base_url);
        let response: Vec<User> = self
            .client
            .get(url)
            .header(
                "Authorization",
                format!("MediaBrowser Token=\"{}\"", self.api_token),
            )
            .send()
            .await?
            .json()
            .await?;
        Ok(response)
    }

    pub async fn views(&self, user_id: &str) -> anyhow::Result<Vec<View>> {
        #[derive(Deserialize)]
        struct ViewList {
            #[serde(rename(deserialize = "Items"))]
            items: Vec<View>,
        }

        let url = format!("{}/Users/{user_id}/Views", self.base_url);
        let response: ViewList = self
            .client
            .get(url)
            .header(
                "Authorization",
                format!("MediaBrowser Token=\"{}\"", self.api_token),
            )
            .send()
            .await?
            .json()
            .await?;
        Ok(response.items)
    }

    pub async fn random_audio(&self, user_id: &str, collection_id: &str) -> anyhow::Result<Audio> {
        #[derive(Deserialize)]
        struct AudioList {
            #[serde(rename(deserialize = "Items"))]
            items: Vec<Audio>,
        }

        let url = format!("{}/Users/{user_id}/Items", self.base_url);
        let mut response: AudioList = self
            .client
            .get(url)
            .query(&[
                ("ParentId", collection_id),
                ("Filters", "IsNotFolder"),
                ("Recursive", "true"),
                ("SortBy", "Random"),
                ("MediaTypes", "Audio"),
                ("Limit", "1"),
                ("ExcludeLocationTypes", "Virtual"),
                ("CollapseBoxSetItems", "false"),
            ])
            .header(
                "Authorization",
                format!("MediaBrowser Token=\"{}\"", self.api_token),
            )
            .send()
            .await?
            .json()
            .await?;
        response.items.pop().ok_or(anyhow::anyhow!("No item found"))
    }

    pub async fn fetch_audio(&self, audio: Audio) -> anyhow::Result<Box<dyn awedio::Sound>> {
        let url = format!("{}/Items/{}/Download", self.base_url, audio.id);
        let response = self
            .client
            .get(url)
            .header(
                "Authorization",
                format!("MediaBrowser Token=\"{}\"", self.api_token),
            )
            .send()
            .await?;
        let filename = response
            .headers()
            .get(reqwest::header::CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(";").into_iter())
            .map(|i| {
                i.filter(|v| v.contains("filename="))
                    .map(|v| v.split("=").collect::<Vec<&str>>()[1])
                    .next()
            })
            .unwrap();
        let extension = filename
            .and_then(|v| v.rsplit(".").next())
            .map(String::from)
            .map(|s| s.replace("\"", ""));
        let body = response.bytes().await?;

        let decoder = Box::new(awedio::sounds::decoders::SymphoniaDecoder::new(
            Box::new(symphonia::core::io::ReadOnlySource::new(body.reader())),
            extension.as_deref(),
        )?);
        
        Ok(decoder)
    }
}
