# jellyfin-radio

Ever dreamt of having your own radio station, but without the hassle of choosing the music yourself?

jellyfin-radio randomly plays Songs from a Jellyfin collection and streams it via .mp3, which can be streamed for example on a Yamaha MusicCast System.

The usage is rather simple. It is recommended to run this as a Docker container. An example docker-compose configuration might look like this:

```yaml
version: "3"
services:
    radio:
        image: ghcr.io/jhbruhn/jellyfin-radio:main
        ports:
            - "3000:3000"
        restart: unless-stopped
        environment:
            JELLYFIN_URL: http://<jellyfin-server>:<jellyfin-port>
            JELLYFIN_API_KEY: <api-key> # generated in jellyfin UI
            JELLYFIN_COLLECTION_NAME: Music # name of the Collection you want to play music from
            SONG_PREFETCH: 2 # optional: Define how many songs jellyfin-radio should fetch in advance
```

# License
MIT