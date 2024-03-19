use awedio::{manager::Manager, Sound};

use async_broadcast::Receiver;
use bytes::Bytes;
use core::time::Duration;
use futures_util::StreamExt;
use futures_util::TryStreamExt;
use http_body_util::{combinators::BoxBody, StreamBody};
use hyper::body::Frame;
use hyper::service::Service;
use hyper::{body, Request};
use hyper::{Response, StatusCode};

const SAMPLE_RATE: u64 = 48000;
const CHANNEL_COUNT: u64 = 2;

const BUFFER_SIZE: usize = 2000; // Should be an integer result of 48000 / 2 / x

type Chunk = [i16; BUFFER_SIZE];

pub struct StreamerBackend {
    stream_receiver: Receiver<Box<Chunk>>,
}

impl StreamerBackend {
    pub fn start() -> anyhow::Result<(Self, Manager)> {
        let (manager, mut renderer) = Manager::new();
        renderer.set_output_channel_count_and_sample_rate(CHANNEL_COUNT as u16, SAMPLE_RATE as u32);

        let Ok(awedio::NextSample::MetadataChanged) = renderer.next_sample() else {
            panic!("expected MetadataChanged event")
        };

        let (mut s, stream_receiver) = async_broadcast::broadcast(3);
        s.set_overflow(true);

        tokio::spawn(async move {
            let mut stream = tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(
                Duration::from_millis(1000 * BUFFER_SIZE as u64 / CHANNEL_COUNT / SAMPLE_RATE),
            ))
            .map(move |_| {
                let mut buffer = [0_i16; BUFFER_SIZE];
                    renderer.on_start_of_batch();
                    tokio::task::block_in_place(|| {
                        buffer.fill_with(|| {
                            let sample = renderer
                                .next_sample()
                                .expect("renderer should never return an Error");
                            let sample = match sample {
                                awedio::NextSample::Sample(s) => s,
                                awedio::NextSample::MetadataChanged => {
                                    unreachable!("we never change metadata mid-batch")
                                }
                                awedio::NextSample::Paused => 0,
                                awedio::NextSample::Finished => 0,
                            };
                            sample
                        });
                    });
                    Box::new(buffer)
                });

            loop {
                s.broadcast(stream.next().await.expect("Should not end!"))
                    .await
                    .unwrap();
            }
        });

        Ok((Self { stream_receiver }, manager))
    }
}

impl Clone for StreamerBackend {
    fn clone(&self) -> Self {
        Self {
            stream_receiver: self.stream_receiver.clone(),
        }
    }
}

impl Service<Request<body::Incoming>> for StreamerBackend {
    type Response = Response<BoxBody<Bytes, anyhow::Error>>;

    type Error = anyhow::Error;

    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn call(&self, _req: Request<body::Incoming>) -> Self::Future {
        use mp3lame_encoder::{Builder, InterleavedPcm};

        let mut mp3_encoder = Builder::new().expect("Create LAME builder");
        mp3_encoder
            .set_num_channels(CHANNEL_COUNT as u8)
            .expect("set channels");
        mp3_encoder
            .set_sample_rate(SAMPLE_RATE as u32)
            .expect("set sample rate");
        mp3_encoder
            .set_brate(mp3lame_encoder::Bitrate::Kbps320)
            .expect("set brate");
        mp3_encoder
            .set_quality(mp3lame_encoder::Quality::Best)
            .expect("set quality");
        let mut mp3_encoder = mp3_encoder.build().expect("To initialize LAME encoder");

        //use actual PCM data
        let watch_stream = self.stream_receiver.clone().map(move |data| {
            let input = InterleavedPcm(&data.as_slice());
            let mut mp3_out_buffer: Vec<u8> = Vec::new();
            mp3_out_buffer.reserve(mp3lame_encoder::max_required_buffer_size(data.len() / 2));
            let encoded_size = mp3_encoder
                .encode(input, mp3_out_buffer.spare_capacity_mut())
                .expect("To encode");
            unsafe {
                mp3_out_buffer.set_len(mp3_out_buffer.len().wrapping_add(encoded_size));
            }
            anyhow::Ok(Bytes::from(mp3_out_buffer))
        });

        let stream_body = StreamBody::new(watch_stream.map_ok(Frame::data));

        let boxed_body: BoxBody<Bytes, anyhow::Error> = BoxBody::new(stream_body); //.boxed();
        Box::pin(async {
            anyhow::Ok(
                Response::builder()
                    .status(StatusCode::OK)
                    .body(boxed_body)
                    .unwrap(),
            )
        })
    }
}
