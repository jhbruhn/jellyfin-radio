use awedio::Sound;
use envconfig::Envconfig;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use std::{net::SocketAddr, time::Duration};
use tokio::net::TcpListener;
use tracing_subscriber::fmt::format::FmtSpan;

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

    #[envconfig(from = "INTERSTITIAL_PATH")]
    pub interstitial_path: Option<String>,
}

async fn get_time_file_map(
    folder: &std::path::Path,
) -> std::collections::HashMap<chrono::NaiveTime, Vec<std::path::PathBuf>> {
    // time announce logic
    let time_files: Vec<std::path::PathBuf> = std::fs::read_dir(folder)
        .unwrap()
        .filter_map(|v| v.ok())
        .filter(|v| !v.path().is_dir())
        .map(|v| v.path().clone())
        .collect();

    let mut time_map = std::collections::HashMap::new();
    for path in time_files.iter() {
        if let Ok((time, path)) = async {
            let mut name_split = path
                .file_stem()
                .ok_or(anyhow::anyhow!("Wrong file stem!"))?
                .to_str()
                .ok_or(anyhow::anyhow!("Wrong file path!"))?
                .split("_");
            let hour = name_split
                .next()
                .ok_or(anyhow::anyhow!("No hour!"))?
                .parse()?;
            let minute = name_split
                .next()
                .ok_or(anyhow::anyhow!("No minute!"))?
                .parse()?;
            let time = chrono::NaiveTime::from_hms_opt(hour, minute, 0)
                .ok_or(anyhow::anyhow!("Can't parse time"))?;
            Ok::<_, anyhow::Error>((time, path))
        }
        .await
        {
            time_map
                .entry(time)
                .or_insert_with(Vec::new)
                .push(path.clone());
        }
    }
    time_map
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = std::env::var("RUST_LOG")
        .unwrap_or_else(|_| "jellyfin_radio=debug,tracing=info,hyper=info".to_owned());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_span_events(FmtSpan::CLOSE)
        .init();
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

    let (mixer, mixer_controller) = awedio::sounds::SoundMixer::new(2, 48_000).controllable();
    // basic playlist playback

    let (player, mut player_controller) = player::Player::new(config.song_prefetch);
    let player = Box::new(player);

    let mut player_mixer_controller = mixer_controller.clone();
    player_mixer_controller.add(player);
    let mut announce_downmix_player_controller = player_controller.clone();
    tokio::task::spawn(async move {
        loop {
            tokio::task::yield_now().await;
            player_controller.wait_for_queue().await;

            tracing::info!("Queuing song");

            loop {
                let result = async {
                    let item = client
                        .random_audio(&admin_user.id, &matched_collection.id)
                        .await?;

                    tracing::info!("Fetching {} - {}", item.artists.join(","), item.name);
                    let sound = client.fetch_audio(item).await?;
                    tracing::info!("Fetched Song!");
                    if sound.channel_count() > 2 {
                        anyhow::bail!("Too many channels, skipping!");
                    }
                    player_controller.add(Box::new(sound));
                    anyhow::Ok(())
                }
                .await;
                if let Err(e) = result {
                    tracing::error!("Error fetching new song: {}", e);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                } else {
                    break;
                }
            }
        }
    });

    let mut time_announce_mixer_controller = mixer_controller.clone();

    tokio::task::spawn(async move {
        if config.interstitial_path.is_none() {
            tracing::info!("No interstitials, skipping interstitial task. Specify a folder with INTERSTITIAL_PATH.");
            return;
        }

        let mut time_file_path = std::path::PathBuf::from(config.interstitial_path.unwrap());
        time_file_path.push("time");
        tracing::info!("Looking for time files at {:?}", time_file_path);

        let time_file_map = get_time_file_map(&time_file_path).await;
        loop {
            tokio::task::yield_now().await;

            let fade_duration = Duration::from_secs(2);
            let fade_steps = 100;
            let fade_minimum_level = 0.1;

            let fade_steps_max = fade_steps;
            let fade_steps_min = (fade_minimum_level * fade_steps as f32) as u32;

            let now = chrono::Local::now();
            use itertools::Itertools;
            let next_time = if let Some(next_time) = time_file_map
                .keys()
                .sorted()
                .filter(|k| **k > now.time())
                .next()
            {
                Some(next_time)
            } else {
                time_file_map.keys().sorted().next()
            };

            if let Some(next_time) = next_time {
                let paths = time_file_map.get(next_time).unwrap(); // definitely exists, we just did the math
                use rand::seq::SliceRandom;
                let next_path = paths.choose(&mut rand::thread_rng());
                if next_path.is_none() {
                    continue;
                }
                let next_path = next_path.unwrap();

                let interstitial_time = if *next_time > now.time() {
                    now.date_naive()
                        .and_time(*next_time)
                        .and_local_timezone(chrono::Local)
                } else {
                    now.date_naive()
                        .checked_add_days(chrono::Days::new(1))
                        .unwrap()
                        .and_time(*next_time)
                        .and_local_timezone(chrono::Local)
                }
                .unwrap();

                tracing::info!(
                    "Next Internstitial time {interstitial_time}: {:?}",
                    next_path
                );

                tokio::time::sleep_until(
                    tokio::time::Instant::now() + (interstitial_time - now).to_std().unwrap(),
                )
                .await;

                tracing::info!("Playing interstitial {:?}", next_path);

                if let Ok(sound) = awedio::sounds::open_file(next_path.as_path()) {
                    let (sound, completion_notifier) = sound.with_async_completion_notifier();
                    for v in (fade_steps_min..=fade_steps_max).rev() {
                        let volume = v as f32 / fade_steps as f32;
                        announce_downmix_player_controller.set_volume(volume);
                        tokio::time::sleep(fade_duration / (fade_steps_max - fade_steps_min)).await;
                    }

                    time_announce_mixer_controller.add(Box::new(sound));
                    let _ = completion_notifier.await;

                    for v in fade_steps_min..=fade_steps_max {
                        let volume = v as f32 / fade_steps as f32;
                        announce_downmix_player_controller.set_volume(volume);
                        tokio::time::sleep(fade_duration / (fade_steps_max - fade_steps_min)).await;
                    }
                } else {
                    tracing::error!("Error playing interstitial");
                }
            }
        }
    });

    streamer_manager.play(Box::new(mixer));

    let listener = TcpListener::bind(addr).await?;
    tracing::info!("Listening on http://{}", addr);
    loop {
        let (tcp, _) = listener.accept().await?;
        let io = TokioIo::new(tcp);
        let backend = streamer_backend.clone();

        tracing::debug!("New connection!");

        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new().serve_connection(io, backend).await {
                tracing::error!("Error serving connection: {:?}", err);
            }
        });
    }
}
