use awedio::NextSample;
use awedio::Sound;
use std::sync::mpsc;
use std::sync::Arc;
use tokio::sync::Notify;

/// Heavily Based on awedios SoundList and Controllable implementations

pub struct Player {
    sounds: Vec<Box<dyn Sound>>,
    was_empty: bool,
    song_prefetch: u32,
    volume_adjustment: f32,
}

type Command<S> = Box<dyn FnOnce(&mut S) + Send>;

pub struct PlayerControllable {
    inner: Player,
    command_receiver: mpsc::Receiver<Command<Player>>,
    queue_next_song_notify: Arc<Notify>,
    finished: bool,
}

pub struct PlayerController {
    command_sender: mpsc::Sender<Command<Player>>,
    queue_next_song_notify: Arc<Notify>,
}

impl Player {
    /// Create a new empty Player.
    pub fn new(song_prefetch: u32) -> (PlayerControllable, PlayerController) {
        let inner = Player {
            sounds: Vec::new(),
            was_empty: false,
            song_prefetch,
            volume_adjustment: 1.0,
        };

        let queue_next_song_notify = Arc::new(tokio::sync::Notify::new());
        let (command_sender, command_receiver) = mpsc::channel::<Command<Player>>();
        let controllable = PlayerControllable {
            inner,
            queue_next_song_notify: queue_next_song_notify.clone(),
            command_receiver,
            finished: false,
        };

        let controller = PlayerController {
            command_sender,
            queue_next_song_notify,
        };

        (controllable, controller)
    }

    /// Add a Sound to be played after any existing sounds have `Finished`.
    pub fn add(&mut self, sound: Box<dyn Sound>) {
        if self.sounds.is_empty() {
            self.was_empty = true;
        }
        self.sounds.push(sound);
    }

    fn set_volume(&mut self, new: f32) {
        self.volume_adjustment = new;
    }

    fn should_prefetch(&self) -> bool {
        self.sounds.len() <= self.song_prefetch as usize
    }
}

// Returned only when no sounds exist so they shouldn't be used in practice.
const DEFAULT_CHANNEL_COUNT: u16 = 2;
const DEFAULT_SAMPLE_RATE: u32 = 44100;

impl Sound for Player {
    fn channel_count(&self) -> u16 {
        self.sounds
            .first()
            .map(|s| s.channel_count())
            .unwrap_or(DEFAULT_CHANNEL_COUNT)
    }

    fn sample_rate(&self) -> u32 {
        self.sounds
            .first()
            .map(|s| s.sample_rate())
            .unwrap_or(DEFAULT_SAMPLE_RATE)
    }

    fn on_start_of_batch(&mut self) {
        for sound in &mut self.sounds {
            sound.on_start_of_batch();
        }
    }

    fn next_sample(&mut self) -> Result<NextSample, awedio::Error> {
        let Some(next_sound) = self.sounds.first_mut() else {
            return Ok(NextSample::Finished);
        };
        if self.was_empty {
            self.was_empty = false;
            return Ok(NextSample::MetadataChanged);
        }

        let next_sample = next_sound.next_sample();
        if let Err(e) = &next_sample {
            tracing::error!("Error playing track: {:?}", e);
        }

        let ret = match next_sample {
            Ok(NextSample::Sample(s)) => {
                NextSample::Sample((s as f32 * self.volume_adjustment) as i16)
            }
            Ok(NextSample::MetadataChanged | NextSample::Paused) => next_sample.unwrap(),
            Ok(NextSample::Finished) | Err(_) => {
                // Just ignore the error
                self.sounds.remove(0);
                if self.sounds.is_empty() {
                    NextSample::Finished
                } else {
                    // The next sample might have different metadata. Instead of
                    // normalizing here let downstream normalize.
                    NextSample::MetadataChanged
                }
            }
        };
        Ok(ret)
    }
}

impl PlayerControllable {
    fn notify_next_song_queue_if_needed(&mut self) {
        if self.inner.should_prefetch() {
            self.queue_next_song_notify.notify_waiters();
        }
    }
}

impl Sound for PlayerControllable {
    fn channel_count(&self) -> u16 {
        self.inner.channel_count()
    }

    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }

    fn next_sample(&mut self) -> Result<awedio::NextSample, awedio::Error> {
        let next = self.inner.next_sample()?;
        match next {
            awedio::NextSample::Sample(_) | awedio::NextSample::Paused => Ok(next),
            awedio::NextSample::MetadataChanged => {
                // Maybe a new track has started
                self.notify_next_song_queue_if_needed();
                Ok(next)
            }
            // Since this is controllable we might add another sound later.
            // Ideally we would do this only if the inner sound can have sounds
            // added to it but I don't think we can branch on S: AddSound here.
            // We could add a Sound::is_addable but lets avoid that until we see
            // a reason why it is necessary.
            awedio::NextSample::Finished => {
                if self.finished {
                    Ok(awedio::NextSample::Finished)
                } else {
                    Ok(awedio::NextSample::Paused)
                }
            }
        }
    }

    fn on_start_of_batch(&mut self) {
        loop {
            match self.command_receiver.try_recv() {
                Ok(command) => command(&mut self.inner),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.finished = true;
                    break;
                }
            }
        }
        self.notify_next_song_queue_if_needed();
        self.inner.on_start_of_batch();
    }
}

impl PlayerController {
    pub fn send_command(&mut self, command: Command<Player>) {
        // Ignore the error since it only happens if the receiver
        // has been dropped which is not expected after it has been
        // sent to the manager.
        let _ = self.command_sender.send(command);
    }
}

impl Clone for PlayerController {
    fn clone(&self) -> Self {
        Self {
            command_sender: self.command_sender.clone(),
            queue_next_song_notify: self.queue_next_song_notify.clone(),
        }
    }
}

impl PlayerController {
    pub fn add(&mut self, sound: Box<dyn Sound>) {
        self.send_command(Box::new(|s: &mut Player| s.add(sound)));
    }

    pub fn set_volume(&mut self, new: f32) {
        self.send_command(Box::new(move |s: &mut Player| s.set_volume(new)));
    }

    pub async fn wait_for_queue(&mut self) {
        self.queue_next_song_notify.notified().await;
    }
}
