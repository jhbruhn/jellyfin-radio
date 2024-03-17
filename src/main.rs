use envconfig::Envconfig;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use std::{net::SocketAddr, time::Duration};
use tokio::net::TcpListener;

mod jellyfin;
mod player;
mod streamer;

#[derive(Envconfig, Clone)]
struct Config {
    #[envconfig(from = "JELLYFIN_URL")]
    pub jellyfin_url: String,

    #[envconfig(from = "JELLYFIN_API_KEY")]
    pub jellyfin_api_key: String,

    #[envconfig(from = "JELLYFIN_COLLECTION_NAME")]
    pub jellyfin_collection_name: String,

    #[envconfig(from = "PORT", default = "3000")]
    pub port: u16,

    #[envconfig(from = "HOST", default = "0.0.0.0")]
    pub host: String,

    #[envconfig(from = "SONG_PREFETCH", default = "2")]
    pub song_prefetch: u32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::init_from_env().unwrap();

    let client =
        jellyfin::JellyfinClient::new(config.jellyfin_url.into(), config.jellyfin_api_key.into());

    let admin_user = client
        .users()
        .await?
        .into_iter()
        .filter(|u| u.policy.is_administrator)
        .next()
        .expect("No Admin user found!");
    let matched_collection = client
        .views(&admin_user.id)
        .await?
        .into_iter()
        .filter(|c| c.name == config.jellyfin_collection_name)
        .next()
        .expect("Collection not found!");

    let addr: SocketAddr = SocketAddr::from((
        config.host.parse::<std::net::Ipv4Addr>().unwrap(),
        config.port,
    ));

    let (streamer_backend, mut streamer_manager) = streamer::StreamerBackend::start()?;

    tokio::task::spawn(async move {
        let (player, mut controller) = player::Player::new(config.song_prefetch);
        let player = Box::new(player);
        streamer_manager.play(player);
        loop {
            controller.wait_for_queue().await;

            println!("Queuing song");

            loop {
                let result = async {
                    let item = client
                        .random_audio(&admin_user.id, &matched_collection.id)
                        .await?;

                    println!("Fetching {} - {}", item.artists.join(","), item.name);
                    let sound = client.fetch_audio(item).await?;
                    println!("Fetched Song!");
                    if sound.channel_count() > 2 {
                        anyhow::bail!("Too many channels");
                    }
                    controller.add(Box::new(sound));
                    anyhow::Ok(())
                }
                .await;
                if let Err(e) = result {
                    println!("Error fetching new song: {}", e);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                } else {
                    break;
                }
            }
        }
    });

    let listener = TcpListener::bind(addr).await?;
    println!("Listening on http://{}", addr);
    loop {
        let (tcp, _) = listener.accept().await?;
        let io = TokioIo::new(tcp);
        let backend = streamer_backend.clone();

        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new().serve_connection(io, backend).await {
                println!("Error serving connection: {:?}", err);
            }
        });
    }
}
